//! Serializable Liquid anchor JSON for the demo CLI.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiquidAnchor {
    /// The witness tx — the Liquid tx that spends the seal and embeds
    /// the tapret commitment. Filled in after broadcast.
    pub txid: String,
    pub internal_key_hex: String,
    pub mpc_root_hex: String,
    /// One entry per protocol committed in the tree.
    pub entries: Vec<AnchorEntry>,
    /// `Layer1::Liquid` per RGB consensus — set explicitly so anyone
    /// reading the JSON knows this is a Liquid anchor by the rgb-consensus
    /// schema, not just by where it happens to be broadcast.
    pub layer1: String,
    /// `ChainNet::LiquidTestnet` (or whichever variant).
    pub chain_net: String,
    pub static_entropy: u64,
    /// Optional seal. When present, verification also confirms that the
    /// witness tx spends this outpoint (full seal closure). When absent,
    /// verification checks the commitment only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seal: Option<crate::seal::LiquidSeal>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnchorEntry {
    /// Hex of the 32-byte protocol id (typically a ContractId).
    pub protocol_id_hex: String,
    /// Hex of the 32-byte message (typically a BundleId).
    pub message_hex: String,
    /// Optional human label for the contract this entry represents.
    pub label: String,
}
