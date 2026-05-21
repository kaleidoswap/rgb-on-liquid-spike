//! Tapret commit + verify on Liquid scriptPubKeys, using
//! `rgb-consensus 0.11`'s internal `dbc` module — the same code that
//! `rgb-lib` uses for Bitcoin anchors.
//!
//! The result: rgb-consensus's TapretFirst commit + verify functions
//! operate on a `bitcoin::ScriptBuf` and a `bitcoin::Transaction`. The
//! P2TR scriptPubKey byte format is identical on Bitcoin and Elements,
//! so the **commit_verify** part — given a scriptPubKey or a tx hex —
//! accepts Liquid output bytes interchangeably.

use anyhow::{Context, Result};
use rgbcore::bitcoin::key::Secp256k1;
use rgbcore::bitcoin::secp256k1::XOnlyPublicKey;
use rgbcore::bitcoin::ScriptBuf;
use rgbcore::commit_verify::{mpc, ConvolveCommit, ConvolveCommitProof};
use rgbcore::dbc::tapret::{TapretFirst, TapretPathProof, TapretProof};

pub struct LiquidCommit {
    /// Post-commitment scriptPubKey bytes (hex, e.g. "5120<Q>").
    pub committed_spk_hex: String,
    /// Proof structure used to verify.
    pub proof: TapretProof,
}

/// Build a tapret commitment for the given (internal P, MPC root).
/// Uses the simplest TapretPathProof (no script-tree partner, nonce=0).
pub fn commit(internal_xonly: [u8; 32], mpc_root: [u8; 32]) -> Result<LiquidCommit> {
    let internal_pk = XOnlyPublicKey::from_slice(&internal_xonly)
        .context("internal_xonly is not a valid x-only key")?;
    let mpc_commitment = mpc::Commitment::from(mpc_root);

    // Pre-commitment scriptPubKey: key-path-only P2TR.
    let secp = Secp256k1::verification_only();
    let original_spk = ScriptBuf::new_p2tr(&secp, internal_pk, None);

    let supplement = TapretProof {
        path_proof: TapretPathProof::root(0),
        internal_pk,
    };

    let (committed_spk, proof) =
        <ScriptBuf as ConvolveCommit<mpc::Commitment, TapretProof, TapretFirst>>::convolve_commit(
            &original_spk,
            &supplement,
            &mpc_commitment,
        )
        .map_err(|e| anyhow::anyhow!("convolve_commit: {e:?}"))?;

    Ok(LiquidCommit {
        committed_spk_hex: hex::encode(committed_spk.as_bytes()),
        proof,
    })
}

/// Verify that `script_pubkey_hex` is a valid TapretFirst commitment
/// to `mpc_root` under `internal_xonly`.
pub fn verify(script_pubkey_hex: &str, mpc_root: [u8; 32], internal_xonly: [u8; 32]) -> Result<()> {
    let bytes = hex::decode(script_pubkey_hex).context("scriptPubKey hex")?;
    let spk = ScriptBuf::from_bytes(bytes);
    let internal_pk = XOnlyPublicKey::from_slice(&internal_xonly).context("internal_xonly")?;
    let proof = TapretProof {
        path_proof: TapretPathProof::root(0),
        internal_pk,
    };
    let mpc_commitment = mpc::Commitment::from(mpc_root);

    <TapretProof as ConvolveCommitProof<mpc::Commitment, ScriptBuf, TapretFirst>>::verify(
        &proof,
        &mpc_commitment,
        &spk,
    )
    .map_err(|e| anyhow::anyhow!("rgb-consensus verify: {e:?}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_offline() {
        let p: [u8; 32] =
            hex::decode("d6889cb081036e0faefa3a35157ad71086b123b2b144b649798b494c300a961d")
                .unwrap()
                .try_into()
                .unwrap();
        let mpc = [0x42u8; 32];
        let c = commit(p, mpc).unwrap();
        verify(&c.committed_spk_hex, mpc, p).unwrap();

        let mut bad = mpc;
        bad[0] ^= 1;
        assert!(verify(&c.committed_spk_hex, bad, p).is_err());
    }
}
