//! Minimal JSON-RPC client for `elementsd` and `bitcoind`.
//!
//! Covers only the methods this project uses (`getblockchaininfo`,
//! `getblockcount`, `getrawtransaction`, `sendrawtransaction`). Both
//! nodes speak the same JSON-RPC wire protocol, so one client serves
//! both; only the URL differs.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug)]
pub struct ElementsRpc {
    url: String,
    user: String,
    pass: String,
    client: reqwest::Client,
}

#[derive(Serialize)]
struct RpcRequest<'a> {
    jsonrpc: &'a str,
    id: &'a str,
    method: &'a str,
    params: Value,
}

#[derive(Deserialize)]
struct RpcResponse {
    result: Option<Value>,
    error: Option<RpcError>,
}

#[derive(Deserialize, Debug)]
struct RpcError {
    code: i64,
    message: String,
}

impl ElementsRpc {
    pub fn new(url: impl Into<String>, user: impl Into<String>, pass: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            user: user.into(),
            pass: pass.into(),
            client: reqwest::Client::new(),
        }
    }

    pub fn from_defaults() -> Self {
        use crate::defaults::*;
        Self::new(ELEMENTSD_RPC_URL, ELEMENTSD_RPC_USER, ELEMENTSD_RPC_PASS)
    }

    /// JSON-RPC client pointed at Bitcoin Core regtest. The wire
    /// protocol is identical to elementsd's, so the same client type
    /// works for both — only the URL differs.
    pub fn bitcoind_defaults() -> Self {
        use crate::defaults::*;
        Self::new(BITCOIND_RPC_URL, ELEMENTSD_RPC_USER, ELEMENTSD_RPC_PASS)
    }

    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let req = RpcRequest {
            jsonrpc: "1.0",
            id: "spike",
            method,
            params,
        };
        let resp: RpcResponse = self
            .client
            .post(&self.url)
            .basic_auth(&self.user, Some(&self.pass))
            .json(&req)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        if let Some(err) = resp.error {
            return Err(anyhow!(
                "rpc {} failed [{}]: {}",
                method,
                err.code,
                err.message
            ));
        }
        resp.result
            .ok_or_else(|| anyhow!("rpc {} returned null", method))
    }

    pub async fn get_block_count(&self) -> Result<u64> {
        Ok(self
            .call("getblockcount", Value::Array(vec![]))
            .await?
            .as_u64()
            .unwrap_or(0))
    }

    pub async fn get_blockchain_info(&self) -> Result<Value> {
        self.call("getblockchaininfo", Value::Array(vec![])).await
    }

    pub async fn send_raw_transaction(&self, hex_tx: &str) -> Result<String> {
        let v = self
            .call("sendrawtransaction", serde_json::json!([hex_tx]))
            .await?;
        v.as_str()
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("sendrawtransaction returned non-string: {v}"))
    }
}
