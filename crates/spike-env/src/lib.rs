//! Shared helpers: a minimal JSON-RPC client and fixture loading.
//!
//! - [`elements_rpc`] — minimal JSON-RPC client for `elementsd` and
//!   `bitcoind` (the wire protocol is identical).
//! - [`fixtures`]     — read `out/asset.json` + `out/addresses.json`
//!   written by `scripts/bootstrap.sh`.
//!
//! Regtest-only.

pub mod elements_rpc;
pub mod fixtures;

pub mod defaults {
    pub const ELEMENTSD_RPC_URL: &str = "http://localhost:7041";
    pub const ELEMENTSD_RPC_USER: &str = "user";
    pub const ELEMENTSD_RPC_PASS: &str = "pass";

    /// Bitcoin Core regtest RPC.
    pub const BITCOIND_RPC_URL: &str = "http://localhost:18443";
}
