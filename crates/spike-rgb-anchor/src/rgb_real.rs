//! Build minimal RGB `Genesis` values stamped onto a chosen `ChainNet`.
//!
//! `Genesis` in rgb-consensus 0.11 has a `chain_net: ChainNet` field,
//! so a `ContractId` derived from a Liquid `Genesis` is a Liquid
//! contract by RGB consensus, with no consensus-layer change required.
//!
//! For full contract issuance with real schema state, see [`crate::rgb20`].

use anyhow::Result;
use rgbcore::commit_verify::{Digest, DigestExt, Sha256};
use rgbcore::{ChainNet, ContractId, Genesis, OpId, Operation};
use strict_encoding::StrictDumb;

/// A `Genesis` stamped onto a chosen `ChainNet`, with its `ContractId`.
///
/// The inner data (codex, assignments) comes from `StrictDumb`; only
/// `chain_net` and a tag-derived timestamp are set. This is enough to
/// derive a real, chain-specific `ContractId` without authoring a
/// full contract.
pub struct RgbContract {
    pub chain_net: ChainNet,
    pub genesis: Genesis,
    pub contract_id: ContractId,
}

pub fn build_contract(chain_net: ChainNet, tag: &str) -> Result<RgbContract> {
    let mut genesis = Genesis::strict_dumb();
    genesis.chain_net = chain_net;
    // Bake a tag-derived hash into the timestamp so distinct tags yield
    // distinct, reproducible contract ids.
    let mut hasher = Sha256::default();
    hasher.update(tag.as_bytes());
    let tag_hash = DigestExt::<32>::finish(hasher);
    // Cast first 8 bytes of tag_hash to i64 (signed timestamp slot).
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&tag_hash[..8]);
    genesis.timestamp = i64::from_be_bytes(bytes);

    let contract_id = genesis.contract_id();
    Ok(RgbContract {
        chain_net,
        genesis,
        contract_id,
    })
}

/// Derive a deterministic `OpId` from a textual tag, for use as the
/// per-transition message slot in an MPC tree. A real consignment uses
/// a `BundleId` computed from a `TransitionBundle`; see `rgb20`.
pub fn synthetic_op_id(tag: &str) -> OpId {
    let mut hasher = Sha256::default();
    hasher.update(b"op-");
    hasher.update(tag.as_bytes());
    let h: [u8; 32] = DigestExt::<32>::finish(hasher);
    OpId::from(h)
}

pub fn to_bytes32(id_array: [u8; 32]) -> [u8; 32] {
    id_array
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn liquid_genesis_yields_distinct_contract_id() {
        let bitcoin = build_contract(ChainNet::BitcoinRegtest, "demo").unwrap();
        let liquid = build_contract(ChainNet::LiquidTestnet, "demo").unwrap();
        // Same tag, different chain — different ContractIds.
        assert_ne!(bitcoin.contract_id, liquid.contract_id);
    }

    #[test]
    fn liquid_chainnet_is_layer1_liquid() {
        use rgbcore::Layer1;
        let c = build_contract(ChainNet::LiquidTestnet, "demo").unwrap();
        assert_eq!(c.genesis.chain_net.layer1(), Layer1::Liquid);
    }
}
