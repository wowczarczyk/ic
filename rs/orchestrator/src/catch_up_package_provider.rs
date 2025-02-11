use crate::error::{OrchestratorError, OrchestratorResult};
use crate::registry_helper::RegistryHelper;
use ic_canister_client::Sender;
use ic_canister_client::{Agent, HttpClient};
use ic_crypto::CryptoComponentForNonReplicaProcess;
use ic_logger::{info, warn, ReplicaLogger};
use ic_protobuf::registry::node::v1::NodeRecord;
use ic_protobuf::types::v1 as pb;
use ic_types::NodeId;
use ic_types::{
    consensus::catchup::{
        CUPWithOriginalProtobuf, CatchUpContentProtobufBytes, CatchUpPackage, CatchUpPackageParam,
    },
    consensus::HasHeight,
    crypto::*,
    RegistryVersion, SubnetId,
};
use ic_utils::fs::write_protobuf_using_tmp_file;
use std::convert::TryFrom;
use std::sync::Arc;
use std::{fs::File, path::PathBuf};
use url::Url;

/// Fetches catch-up packages from peers and local storage.
///
/// CUPs are used to determine which version of the IC peers are running
/// and hence which version of the IC this node should be starting.
#[derive(Clone)]
pub(crate) struct CatchUpPackageProvider {
    registry: Arc<RegistryHelper>,
    cup_dir: PathBuf,
    client: HttpClient,
    crypto: Arc<dyn CryptoComponentForNonReplicaProcess + Send + Sync>,
    logger: ReplicaLogger,
    node_id: NodeId,
}

impl CatchUpPackageProvider {
    /// Instantiate a new `CatchUpPackageProvider`
    pub(crate) fn new(
        registry: Arc<RegistryHelper>,
        cup_dir: PathBuf,
        crypto: Arc<dyn CryptoComponentForNonReplicaProcess + Send + Sync>,
        logger: ReplicaLogger,
        node_id: NodeId,
    ) -> Self {
        Self {
            node_id,
            registry,
            cup_dir,
            client: HttpClient::new(),
            crypto,
            logger,
        }
    }

    // Randomly selects a peer from the subnet and pulls its CUP. If this CUP is
    // newer than the currently available one and it could be verified, then this
    // CUP is returned. Note that it is acceptable to use a single peer, because
    // CUPs are validated. If all `f` nodes serve unusable CUPs, we have a probability
    // of 2/3 to hit a non-faulty node, so roughly on 4th attempt we should obtain
    // the correct peer CUP.
    async fn get_peer_cup(
        &self,
        subnet_id: SubnetId,
        registry_version: RegistryVersion,
        current_cup: Option<&CUPWithOriginalProtobuf>,
    ) -> Option<CUPWithOriginalProtobuf> {
        use ic_registry_client_helpers::subnet::SubnetTransportRegistry;
        use rand::seq::SliceRandom;

        let mut nodes: Vec<(NodeId, NodeRecord)> = self
            .registry
            .registry_client
            .get_subnet_transport_infos(subnet_id, registry_version)
            .ok()
            .flatten()
            .unwrap_or_default();
        // Randomize the order of peer_urls
        nodes.shuffle(&mut rand::thread_rng());
        let current_node = nodes
            .as_slice()
            .iter()
            .find(|t| t.0 == self.node_id)
            .cloned();

        // Try only one peer at-a-time if there is already a local CUP,
        // Otherwise, try not to fall back to the registry CUP.
        let mut peers = match current_cup {
            Some(_) => vec![nodes.pop().or_else(|| {
                warn!(
                    self.logger,
                    "Empty peer list for subnet {} at version {}", subnet_id, registry_version
                );
                None
            })?],
            None => nodes,
        };

        // If we are still a member of the subnet, append our own data so that we first try to
        // fetch the CUP from our own replica. This improves the upgrade behaviour of a healthy
        // subnet, as we decrease the probability hitting peers who already started the upgrade
        // process and will not serve a CUP until they're online again.
        if let Some(current_node) = current_node {
            peers.push(current_node);
        }

        let param = current_cup.map(CatchUpPackageParam::from);
        for (_, node_record) in peers.iter().rev() {
            let peer_cup = self
                .fetch_verify_and_deserialize_catch_up_package(node_record, param, subnet_id)
                .await;
            // Note: None is < Some(_)
            if peer_cup.as_ref().map(CatchUpPackageParam::from) > param {
                return peer_cup;
            }
        }
        None
    }

    // Download CUP from the given node.
    //
    // If `param` is given, download only CUPs that are newer than the
    // given CUP. This avoids unnecessary CUP downloads and hence reduces
    // network bandwidth requirements.
    //
    // Also checks the signature of the downloaded catch up package.
    async fn fetch_verify_and_deserialize_catch_up_package(
        &self,
        node_record: &NodeRecord,
        param: Option<CatchUpPackageParam>,
        subnet_id: SubnetId,
    ) -> Option<CUPWithOriginalProtobuf> {
        let http = node_record.clone().http.or_else(|| {
            warn!(
                self.logger,
                "Node record's http endpoint is None: {:?}", node_record
            );
            None
        })?;
        let url_str = format!("http://[{}]:{}", http.ip_addr, http.port);
        let url = Url::parse(&url_str)
            .map_err(|err| {
                warn!(
                    self.logger,
                    "Unable to parse the peer url {}: {:?}", url_str, err
                );
            })
            .ok()?;

        let protobuf = self.fetch_catch_up_package(url.clone(), param).await?;
        let cup = CUPWithOriginalProtobuf {
            cup: CatchUpPackage::try_from(&protobuf)
                .map_err(|e| {
                    warn!(
                        self.logger,
                        "Failed to read CUP from peer at url {}: {:?}", url, e
                    )
                })
                .ok()?,
            protobuf,
        };
        self.crypto
            .verify_combined_threshold_sig_by_public_key(
                &CombinedThresholdSigOf::new(CombinedThresholdSig(cup.protobuf.signature.clone())),
                &CatchUpContentProtobufBytes(cup.protobuf.content.clone()),
                subnet_id,
                cup.cup.content.block.get_value().context.registry_version,
            )
            .map_err(|e| {
                warn!(
                    self.logger,
                    "Failed to verify CUP signature at: {:?} with: {:?}", url, e
                )
            })
            .ok()?;
        Some(cup)
    }

    // Attempt to fetch a `CatchUpPackage` from the given endpoint.
    //
    // Does not check the signature of the CUP. This has to be done by the
    // caller.
    async fn fetch_catch_up_package(
        &self,
        url: Url,
        param: Option<CatchUpPackageParam>,
    ) -> Option<pb::CatchUpPackage> {
        Agent::new_with_client(self.client.clone(), url, Sender::Anonymous)
            .query_cup_endpoint(param)
            .await
            .map_err(|e| warn!(self.logger, "Failed to query CUP endpoint: {:?}", e))
            .ok()?
    }

    /// Persist the given CUP to disk.
    ///
    /// This is necessary, as it allows the orchestrator to find a CUP
    /// it previously downloaded again after restart, so that the node
    /// manager never goes back in time.  It will always find a CUP
    /// that is at least as high as the one it has previously
    /// discovered.
    ///
    /// Follows guidelines for DFINITY thread-safe I/O.
    pub(crate) fn persist_cup(&self, cup: &CUPWithOriginalProtobuf) -> OrchestratorResult<PathBuf> {
        let cup_file_path = self.get_cup_path();
        info!(
            self.logger,
            "Persisting CUP (registry version={}, height={}) to file {}",
            cup.cup.content.registry_version(),
            cup.cup.height(),
            &cup_file_path.display(),
        );
        write_protobuf_using_tmp_file(&cup_file_path, &cup.protobuf).map_err(|e| {
            OrchestratorError::IoError(
                format!("Failed to serialize protobuf to disk: {:?}", &cup_file_path),
                e,
            )
        })?;

        Ok(cup_file_path)
    }

    /// The path that should be used to save the CUP for the assigned subnet.
    /// Includes the specific type encoded in the file for future-proofing and
    /// ease of debugging.
    pub fn get_cup_path(&self) -> PathBuf {
        self.cup_dir.join("cup.types.v1.CatchUpPackage.pb")
    }

    /// Return the most up to date CUP.
    ///
    /// Choose the highest CUP among: those provided by the subnet peers,
    /// the locally persisted CUP (if one exists) and the CUP that is specified
    /// by the registry. If we manage to find a newer CUP we also persist it.
    pub(crate) async fn get_latest_cup(
        &self,
        local_cup: Option<CUPWithOriginalProtobuf>,
        subnet_id: SubnetId,
    ) -> OrchestratorResult<CUPWithOriginalProtobuf> {
        let registry_version = self.registry.get_latest_version();
        let local_cup_height = local_cup.as_ref().map(|cup| cup.cup.content.height());

        let subnet_cup = self
            .get_peer_cup(subnet_id, registry_version, local_cup.as_ref())
            .await;

        let registry_cup = self
            .registry
            .get_registry_cup(registry_version, subnet_id)
            .map(CUPWithOriginalProtobuf::from_cup)
            .map_err(|err| warn!(self.logger, "Failed to retrieve registry CUP {:?}", err))
            .ok();

        let latest_cup = vec![local_cup, registry_cup, subnet_cup]
            .into_iter()
            .flatten()
            .max_by_key(|cup| cup.cup.content.height())
            .ok_or(OrchestratorError::MakeRegistryCupError(
                subnet_id,
                registry_version,
            ))?;

        let unsigned = latest_cup.cup.signature.signature.get_ref().0.is_empty();
        let height = Some(latest_cup.cup.content.height());
        // We recreate the local registry CUP everytime to avoid incompatibility issues. Without
        // this recreation, we might run into the following problem: assume the orchestrator of
        // version A creates a local unsigned CUP from the registry contents, persists it, then
        // detects a new replica version B, upgrades to it and starts the replica on the previously
        // created CUP. Now since such a case might happen on a new subnet creation or during a
        // subnet recover with failover nodes, all nodes before upgrading to B might have been on
        // different versions and hence might have created different CUPs, which are then consumed
        // by the same replica version B, which is not guaranteed to be deterministic.
        //
        // By re-creating the unsigned CUP every time we realize it's the newest one, we instead
        // recreate the CUP on all orchestrators of the version B before starting the replica.
        if height > local_cup_height || height == local_cup_height && unsigned {
            self.persist_cup(&latest_cup)?;
        }

        Ok(latest_cup)
    }

    /// Returns the locally persisted CUP.
    pub fn get_local_cup(&self) -> Option<CUPWithOriginalProtobuf> {
        let path = self.get_cup_path();
        if !path.exists() {
            return None;
        }
        match File::open(&path) {
            Ok(reader) => pb::CatchUpPackage::read_from_reader(reader)
                .and_then(|protobuf| {
                    Ok(CUPWithOriginalProtobuf {
                        cup: CatchUpPackage::try_from(&protobuf)?,
                        protobuf,
                    })
                })
                .map_err(|e| warn!(self.logger, "Failed to read CUP from file {:?}", e))
                .ok(),
            Err(err) => {
                warn!(self.logger, "Couldn't open file {:?}: {:?}", path, err);
                None
            }
        }
    }
}
