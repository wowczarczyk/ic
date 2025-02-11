#!/bin/bash

##: nns_get_info
## Prints the info for a named NNS canister
## Usage: $1 <NETWORK> <CANISTER_NAME>
##      NETWORK: ic, or URL to an NNS subnet (including port)
##      CANISTER_NAME: human readable canister name (i.e. governance, registry, sns-wasm, etc...)
nns_get_info() {
    local NETWORK=$1
    local CANISTER_NAME=$2

    get_info "$NETWORK" $(nns_canister_id "$CANISTER_NAME")
}

get_info() {
    local NETWORK=$1
    local CANISTER_ID=$2

    dfx canister --network "$NETWORK" info "$CANISTER_ID"
}

nns_canister_hash() {
    local NETWORK=$1
    local CANISTER_NAME=$2

    canister_hash "$NETWORK" $(nns_canister_id "$CANISTER_NAME")
}

canister_hash() {
    local NETWORK=$1
    local CANISTER_ID=$2

    get_info "$NETWORK" "$CANISTER_ID" \
        | grep "Module hash:" \
        | cut -d" " -f3 \
        | sed 's/^0x//'
}

##: nns_canister_git_version
## Gets the git_commit_id from an NNS  canister's metadata if set, looked up by human-friendly name
## Usage: $1 <NETWORK> <CANISTER_NAME>
##      NETWORK: ic, or URL to an NNS subnet (including port)
##      CANISTER_NAME: human readable canister name (i.e. governance, registry, sns-wasm, etc...)
nns_canister_git_version() {
    local NETWORK=$1
    local CANISTER_NAME=$2

    canister_git_version "$NETWORK" $(nns_canister_id "$CANISTER_NAME")
}

##: canister_git_version
## Gets the git_commit_id from the canister's metadata if set
## Usage: $1 <NETWORK> <CANISTER_ID>
##      NETWORK: ic, or URL to an NNS subnet (including port)
##      CANISTER_ID: CanisterId for the canister (a Canister principal)
canister_git_version() {
    local NETWORK=$1
    local CANISTER_ID=$2

    dfx canister --network "$NETWORK" metadata \
        "$CANISTER_ID" git_commit_id
}

nns_canister_id() {
    CANISTER_NAME=$1

    IC_REPO=$(repo_root)
    pushd "$IC_REPO/rs/nns" >/dev/null

    cat ./canister_ids.json \
        | jq -er ".[\"$CANISTER_NAME\"].mainnet" \
        | grep -v null

    FOUND=$?

    popd >/dev/null

    return $FOUND
}

##: canister_has_version_installed
## Check if canister has the right version (git commit)
##      NETWORK: ic, or URL to an NNS subnet (including port)
##      CANISTER_NAME: human readable canister name (i.e. governance, registry, sns-wasm, etc...)
##      VERSION: Git hash of expected version
canister_has_version_installed() {
    local NETWORK=$1
    local CANISTER_NAME=$2
    local VERSION=$3

    WASM_GZ=$(get_nns_canister_wasm_gz_for_type "$CANISTER_NAME" "$VERSION")

    canister_has_file_contents_installed "$NETWORK" "$CANISTER_NAME" "$WASM_GZ"
}

canister_has_file_contents_installed() {
    local NETWORK=$1
    local CANISTER_NAME=$2
    local WASM_FILE=$3

    echo "Checking if canister $CANISTER_NAME is running $WASM_FILE..."

    WASM_HASH=$(sha_256 "$WASM_FILE")
    RUNNING_HASH=$(nns_canister_hash "$NETWORK" "$CANISTER_NAME")

    if [ "$WASM_HASH" != "$RUNNING_HASH" ]; then
        echo >&2 "Canister has hash $RUNNING_HASH; expected $WASM_HASH"
        return 1
    fi

    echo >&2 "Canister is running with hash $WASM_HASH as expected"
    return 0
}
