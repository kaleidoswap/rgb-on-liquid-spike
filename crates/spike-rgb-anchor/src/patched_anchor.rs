//! Verify an RGB anchor against a Liquid transaction through the
//! patched `rgbcore::dbc::Anchor::verify`.
//!
//! `Anchor::verify(protocol_id, message, &witness)` is the tx-level API
//! the `WitnessTx` patch makes generic. Calling it successfully on a
//! Liquid `WitnessTx` shows the patch is sufficient: the same upstream
//! verification path accepts a Liquid transaction unchanged.

use anyhow::Result;
use rgbcore::bitcoin::secp256k1::XOnlyPublicKey;
use rgbcore::commit_verify::mpc;
use rgbcore::dbc::tapret::{TapretPathProof, TapretProof};
use rgbcore::dbc::Anchor;

use crate::seal::WitnessTx as LiquidWitness;

/// Construct a single-entry `Anchor<TapretProof>` for the given
/// `(protocol_id, message)` pair.
pub fn build_anchor(
    protocol_id: [u8; 32],
    message: [u8; 32],
    static_entropy: u64,
    internal_xonly: [u8; 32],
) -> Result<Anchor<TapretProof>> {
    use rgbcore::commit_verify::TryCommitVerify;

    // Build a single-entry MPC tree.
    let mut multi_source = mpc::MultiSource {
        static_entropy: Some(static_entropy),
        ..Default::default()
    };
    multi_source
        .messages
        .insert(
            mpc::ProtocolId::from(protocol_id),
            mpc::Message::from(message),
        )
        .map_err(|e| anyhow::anyhow!("MultiSource::insert: {e:?}"))?;
    let tree = mpc::MerkleTree::try_commit(&multi_source)
        .map_err(|e| anyhow::anyhow!("MerkleTree::try_commit: {e:?}"))?;

    // Inclusion proof for our one protocol.
    let block = mpc::MerkleBlock::from(&tree);
    let mpc_proof = block
        .to_merkle_proof(mpc::ProtocolId::from(protocol_id))
        .map_err(|e| anyhow::anyhow!("to_merkle_proof: {e:?}"))?;

    // The DBC proof side: naked tapret (no script-tree partner, nonce=0).
    let internal_pk = XOnlyPublicKey::from_slice(&internal_xonly)
        .map_err(|e| anyhow::anyhow!("XOnlyPublicKey: {e:?}"))?;
    let dbc_proof = TapretProof {
        path_proof: TapretPathProof::root(0),
        internal_pk,
    };

    Ok(Anchor::new(mpc_proof, dbc_proof))
}

/// Use the patched `Anchor::verify` against a Liquid `WitnessTx`.
/// On success, returns the MPC commitment (the root). On failure,
/// returns the upstream VerifyError stringified.
pub fn verify_anchor_on_liquid(
    anchor: &Anchor<TapretProof>,
    protocol_id: [u8; 32],
    message: [u8; 32],
    witness: &LiquidWitness,
) -> Result<[u8; 32]> {
    let pid = mpc::ProtocolId::from(protocol_id);
    let msg = mpc::Message::from(message);
    let commitment = anchor
        .verify(pid, msg, witness)
        .map_err(|e| anyhow::anyhow!("rgbcore::dbc::Anchor::verify: {e:?}"))?;
    Ok(commitment.to_byte_array())
}
