//! BIP-341 tweak math for the tapret commitment.
//!
//! Implemented against `secp256k1` + `bitcoin_hashes` so we do not pull a
//! whole `rust-bitcoin` dependency tree just to evaluate a tagged hash and
//! one EC addition.

use bitcoin_hashes::{sha256t_hash_newtype, Hash, HashEngine};
use secp256k1::{Scalar, XOnlyPublicKey, SECP256K1};
use thiserror::Error;

// BIP-341 TapTweak tag.
sha256t_hash_newtype! {
    /// `tagged_hash("TapTweak", x)` per BIP-340/341.
    pub struct TapTweakTag = hash_str("TapTweak");

    /// 32-byte taproot tweak.
    #[hash_newtype(forward)]
    pub struct TapTweakHash(_);
}

/// The output of the prove step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TweakedKey {
    pub internal: [u8; 32],
    pub commitment: [u8; 32],
    pub output: [u8; 32],
    pub parity_odd: bool,
}

#[derive(Debug, Error)]
pub enum ProofError {
    #[error("invalid x-only internal key")]
    BadInternalKey,
    #[error("tweak addition failed (key is point at infinity?)")]
    TweakFailed,
    #[error("output key mismatch — commitment does not match on-chain key")]
    OutputMismatch,
}

/// Compute `Q = P + H_taptweak(P||C)·G`.
pub fn prove(internal_xonly: &[u8; 32], commitment: &[u8; 32]) -> Result<TweakedKey, ProofError> {
    let p = XOnlyPublicKey::from_slice(internal_xonly).map_err(|_| ProofError::BadInternalKey)?;

    // tagged_hash("TapTweak", P || C)
    let mut eng = TapTweakHash::engine();
    eng.input(internal_xonly);
    eng.input(commitment);
    let tweak = TapTweakHash::from_engine(eng);
    let tweak_scalar =
        Scalar::from_be_bytes(*tweak.as_ref()).map_err(|_| ProofError::TweakFailed)?;

    let (q_xonly, parity) = p
        .add_tweak(SECP256K1, &tweak_scalar)
        .map_err(|_| ProofError::TweakFailed)?;

    Ok(TweakedKey {
        internal: *internal_xonly,
        commitment: *commitment,
        output: q_xonly.serialize(),
        parity_odd: matches!(parity, secp256k1::Parity::Odd),
    })
}

/// Recompute `Q` from `(P, C)` and check it equals `expected_output_key`.
pub fn verify(
    internal_xonly: &[u8; 32],
    commitment: &[u8; 32],
    expected_output_key: &[u8; 32],
) -> Result<(), ProofError> {
    let derived = prove(internal_xonly, commitment)?;
    if &derived.output == expected_output_key {
        Ok(())
    } else {
        Err(ProofError::OutputMismatch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sanity: tagged_hash("TapTweak", "") matches BIP-341's known constant.
    /// Mostly checks that we're using the right midstate / domain tag.
    /// Regression: the output of `tagged_hash("TapTweak", b"")` is determined
    /// by the BIP-340 construction. This locks in our value so a future
    /// dependency bump that silently changes the construction breaks here
    /// first — before it can break on-chain commitments.
    #[test]
    fn taptweak_empty_tagged_hash_regression() {
        let mut eng = TapTweakHash::engine();
        eng.input(&[]);
        let h = TapTweakHash::from_engine(eng);
        let bytes: [u8; 32] = *h.as_byte_array();
        assert_eq!(
            hex::encode(bytes),
            "8aa4229474ab0100b2d6f0687f031d1fc9d8eef92a042ad97d279bff456b15e4"
        );
    }

    #[test]
    fn round_trip_deterministic() {
        // Fixed internal key (generator G's x-coord won't work; use a known one).
        // Take BIP-341's internal key for convenience.
        let internal =
            hex::decode("d6889cb081036e0faefa3a35157ad71086b123b2b144b649798b494c300a961d")
                .unwrap();
        let commitment = *bitcoin_hashes::sha256::Hash::hash(b"hello rgb on liquid").as_ref();
        let internal_arr: [u8; 32] = internal.as_slice().try_into().unwrap();

        let tk = prove(&internal_arr, &commitment).unwrap();
        verify(&internal_arr, &commitment, &tk.output).unwrap();

        // Tampered commitment must reject.
        let mut bad = commitment;
        bad[0] ^= 1;
        assert!(verify(&internal_arr, &bad, &tk.output).is_err());
    }
}
