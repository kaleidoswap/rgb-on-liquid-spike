//! MPC (LNPBP-4) tree built with `rgb-consensus` 0.11.
//!
//! Multi-entry support: any number of `(ProtocolId, Message)` pairs
//! can live in one tree. The tree's root commitment is what gets
//! placed in the Liquid taproot output via bp-dbc TapretFirst.

use anyhow::{Context, Result};
use rgbcore::commit_verify::{mpc, CommitId, TryCommitVerify};

/// One protocol entry in the MPC tree.
#[derive(Clone, Debug)]
pub struct Entry {
    pub protocol_id: [u8; 32],
    pub message: [u8; 32],
}

/// Build an MPC tree from one or more entries. `entropy` makes the
/// resulting tree deterministic; in production it's randomly sampled
/// per-tree and carried in the consignment.
pub fn build(entries: &[Entry], entropy: u64) -> Result<([u8; 32], mpc::MerkleTree)> {
    if entries.is_empty() {
        anyhow::bail!("MPC tree requires ≥1 entry");
    }
    let mut multi_source = mpc::MultiSource {
        static_entropy: Some(entropy),
        ..Default::default()
    };

    for e in entries {
        let pid = mpc::ProtocolId::from(e.protocol_id);
        let msg = mpc::Message::from(e.message);
        multi_source
            .messages
            .insert(pid, msg)
            .map_err(|err| anyhow::anyhow!("MultiSource::insert: {err:?}"))
            .context("inserting protocol message")?;
    }

    let tree = mpc::MerkleTree::try_commit(&multi_source)
        .map_err(|err| anyhow::anyhow!("MerkleTree::try_commit: {err:?}"))?;

    let cid = tree.commit_id();
    let root: [u8; 32] = cid.to_byte_array();
    Ok((root, tree))
}

/// For a multi-entry tree, produce an inclusion proof for one specific
/// protocol id. A verifier with only the proof + the leaf (pid, msg) +
/// the on-chain root can confirm membership without seeing the whole
/// tree — i.e. the receiver doesn't learn about sibling protocols.
pub fn inclusion_proof(tree: &mpc::MerkleTree, protocol_id: [u8; 32]) -> Result<mpc::MerkleProof> {
    let pid = mpc::ProtocolId::from(protocol_id);
    let block = mpc::MerkleBlock::from(tree);
    block
        .to_merkle_proof(pid)
        .map_err(|e| anyhow::anyhow!("to_merkle_proof: {e:?}"))
}

/// Verify a single-entry inclusion proof against a claimed root.
pub fn verify_inclusion(
    proof: &mpc::MerkleProof,
    protocol_id: [u8; 32],
    message: [u8; 32],
    claimed_root: [u8; 32],
) -> Result<()> {
    let pid = mpc::ProtocolId::from(protocol_id);
    let msg = mpc::Message::from(message);
    let reconstructed_block = mpc::MerkleBlock::with(proof, pid, msg)
        .map_err(|e| anyhow::anyhow!("MerkleBlock::with: {e:?}"))?;
    let root: [u8; 32] = reconstructed_block.commit_id().to_byte_array();
    if root != claimed_root {
        anyhow::bail!(
            "MPC root mismatch: rebuilt {} != claimed {}",
            hex::encode(root),
            hex::encode(claimed_root)
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(p: u8, m: u8) -> Entry {
        Entry {
            protocol_id: [p; 32],
            message: [m; 32],
        }
    }

    #[test]
    fn single_entry_deterministic() {
        let (r1, _) = build(&[e(0xAA, 0x42)], 42).unwrap();
        let (r2, _) = build(&[e(0xAA, 0x42)], 42).unwrap();
        assert_eq!(r1, r2);
    }

    #[test]
    fn multi_entry_root_changes_with_each_entry() {
        let (r1, _) = build(&[e(1, 11), e(2, 22)], 7).unwrap();
        let (r2, _) = build(&[e(1, 11), e(2, 23)], 7).unwrap();
        assert_ne!(r1, r2);
    }

    #[test]
    fn inclusion_proof_round_trip() {
        let entries = vec![e(1, 11), e(2, 22), e(3, 33)];
        let (root, tree) = build(&entries, 99).unwrap();

        // Receiver of contract #2 gets a proof for their entry only.
        let proof = inclusion_proof(&tree, [2; 32]).unwrap();
        verify_inclusion(&proof, [2; 32], [22; 32], root).unwrap();

        // Tampering the message must reject.
        let bad = verify_inclusion(&proof, [2; 32], [99; 32], root);
        assert!(bad.is_err());

        // Claiming a different protocol_id with same proof must reject.
        let bad2 = verify_inclusion(&proof, [99; 32], [22; 32], root);
        assert!(bad2.is_err());
    }
}
