use serde::{Deserialize, Serialize};
use std::{net::SocketAddr, path::PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Exporter {
    /// Log metrics at `TRACE` level every 30 seconds.
    Log,
    /// Expose Prometheus metrics on the specified address.
    Http(SocketAddr),
    /// Dump metrics to the given file on shutdown.
    File(PathBuf),
}

impl Default for Config {
    fn default() -> Self {
        Self {
            exporter: Exporter::Log,
            connection_read_timeout_seconds: 300, // 5 min
            max_tcp_connections: 20,
            max_concurrent_requests: 50,
            request_timeout_seconds: 30,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub exporter: Exporter,

    /// The endpoint can serve from at most 'max_tcp_connections'
    /// simultaneous TCP connections. If the limit is reached and a new
    /// TCP connection arrives, it is accepted and dropped immediately.
    pub max_tcp_connections: usize,

    /// If no bytes are read from a connection for the duration of
    /// 'connection_read_timeout_seconds', then the connection is dropped.
    /// There is no point is setting a timeout on the write bytes since
    /// they are conditioned on the received requests.
    pub connection_read_timeout_seconds: u64,

    /// There can be at most 'max_concurrent_requests' in-flight requests.
    pub max_concurrent_requests: usize,

    /// Per request timeout in seconds before the server replies with 504 Gateway Timeout.
    pub request_timeout_seconds: u64,
}
