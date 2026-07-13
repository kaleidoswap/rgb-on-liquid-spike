//! Cross-chain RGB atomic swap primitives.
//!
//! The atomic link between the two legs of a cross-chain swap is a
//! **hashlock**: a P2WSH output whose witness script is
//!
//! ```text
//! OP_SHA256 <H> OP_EQUAL
//! ```
//!
//! Anyone who knows the preimage `s` such that `SHA256(s) == H` can
//! spend it. When they do, the spending transaction's witness carries
//! `s` in cleartext on-chain, which is what makes the swap atomic:
//! claiming one leg necessarily reveals `s` to the counterparty for the
//! other leg.
//!
//! This is the *minimal* hashlock: no pubkey binding, no timeout
//! branch. It is sufficient to demonstrate the preimage-reveal
//! mechanic. A production HTLC additionally binds a claimer key and a
//! refund timeout:
//! `OP_IF OP_SHA256 <H> OP_EQUALVERIFY <claimerPk> OP_CHECKSIG
//!  OP_ELSE <T> OP_CSV OP_DROP <refundPk> OP_CHECKSIG OP_ENDIF`.
//!
//! The same witness-script bytes produce a valid P2WSH on both Bitcoin
//! and Elements/Liquid; only the address HRP and the transaction
//! envelope differ. This module builds the hashlock and the spend tx
//! for both chains.

use anyhow::Result;
use rgbcore::bitcoin::hashes::{sha256, Hash};

/// The witness script for a minimal hashlock: `OP_SHA256 <H> OP_EQUAL`.
pub fn hashlock_witness_script(hash: &[u8; 32]) -> Vec<u8> {
    let mut s = Vec::with_capacity(35);
    s.push(0xa8); // OP_SHA256
    s.push(0x20); // push 32 bytes
    s.extend_from_slice(hash);
    s.push(0x87); // OP_EQUAL
    s
}

/// The P2WSH scriptPubKey committing to the hashlock witness script:
/// `OP_0 <sha256(witnessScript)>`.
pub fn hashlock_spk(hash: &[u8; 32]) -> Vec<u8> {
    let ws = hashlock_witness_script(hash);
    let wsh = sha256::Hash::hash(&ws);
    let mut spk = Vec::with_capacity(34);
    spk.push(0x00); // OP_0
    spk.push(0x20); // push 32
    spk.extend_from_slice(wsh.as_byte_array());
    spk
}

/// `SHA256(preimage)`.
pub fn hash_of(preimage: &[u8]) -> [u8; 32] {
    *sha256::Hash::hash(preimage).as_byte_array()
}

/// Encode a P2WSH as a bech32 address for the chosen network HRP
/// (witness v0, so bech32 — NOT bech32m).
pub fn hashlock_address(network_hrp: &str, hash: &[u8; 32]) -> Result<String> {
    use bech32::{hrp::Hrp, segwit};
    let ws = hashlock_witness_script(hash);
    let wsh = sha256::Hash::hash(&ws);
    let hrp = Hrp::parse(network_hrp).map_err(|e| anyhow::anyhow!("hrp: {e}"))?;
    let v0 = bech32::Fe32::try_from(0u8).unwrap();
    segwit::encode(hrp, v0, wsh.as_byte_array()).map_err(|e| anyhow::anyhow!("bech32 encode: {e}"))
}

pub mod bitcoin_leg;
pub mod elements_leg;
pub mod htlc;

/// Verify that a preimage matches a hash — used by the counterparty
/// after extracting the preimage from the first leg's claim tx.
pub fn check_preimage(preimage: &[u8], expected_hash: &[u8; 32]) -> Result<()> {
    let got = hash_of(preimage);
    if &got != expected_hash {
        anyhow::bail!(
            "preimage mismatch: SHA256(preimage)={} != expected {}",
            hex::encode(got),
            hex::encode(expected_hash)
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashlock_spk_is_p2wsh_shaped() {
        let h = hash_of(b"swap-secret");
        let spk = hashlock_spk(&h);
        assert_eq!(spk.len(), 34);
        assert_eq!(spk[0], 0x00);
        assert_eq!(spk[1], 0x20);
    }

    #[test]
    fn address_hrp_distinguishes_chains() {
        let h = hash_of(b"swap-secret");
        let btc = hashlock_address("bcrt", &h).unwrap();
        let lq = hashlock_address("ert", &h).unwrap();
        assert!(btc.starts_with("bcrt1q"), "got {btc}");
        assert!(lq.starts_with("ert1q"), "got {lq}");
        // Same witness program — decode both and compare the program
        // bytes (the bech32 checksum mixes in the HRP, so the encoded
        // strings differ even though the program is identical).
        let (_, _, btc_prog) = bech32::segwit::decode(&btc).unwrap();
        let (_, _, lq_prog) = bech32::segwit::decode(&lq).unwrap();
        assert_eq!(btc_prog, lq_prog);
        assert_eq!(btc_prog.len(), 32);
    }

    #[test]
    fn preimage_check_round_trip() {
        let h = hash_of(b"correct horse battery staple");
        check_preimage(b"correct horse battery staple", &h).unwrap();
        assert!(check_preimage(b"wrong", &h).is_err());
    }
}
