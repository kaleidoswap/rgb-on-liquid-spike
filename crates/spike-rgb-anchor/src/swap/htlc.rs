//! Full Hash Time-Locked Contract for the cross-chain swap legs.
//!
//! This is the production-shaped successor to the minimal hashlock in
//! [`super`]: the claim branch binds a claimer key, and a CSV timeout
//! branch lets the funder recover the coins if the counterparty never
//! claims.
//!
//! ```text
//! OP_IF
//!     OP_SHA256 <H> OP_EQUALVERIFY <claimerPk> OP_CHECKSIG
//! OP_ELSE
//!     <T> OP_CHECKSEQUENCEVERIFY OP_DROP <refundPk> OP_CHECKSIG
//! OP_ENDIF
//! ```
//!
//! Claim witness:  `[<claimerSig>, <preimage>, 0x01, witnessScript]`
//! Refund witness: `[<refundSig>, <>, witnessScript]` with the input
//! sequence set to `T` (BIP-68 relative blocks) and tx version 2.
//!
//! As with the minimal hashlock, the same witness-script bytes are
//! valid P2WSH on both Bitcoin and Elements/Liquid: OP_SHA256, OP_CSV
//! and OP_CHECKSIG have identical opcodes and semantics on both
//! chains. Only the transaction envelope and the sighash algorithm
//! implementation differ, so this module carries one script builder
//! and two per-chain spend builders.
//!
//! The spend builders accept an optional **anchor output** (a
//! scriptPubKey and value) inserted at vout 0. That is what lets the
//! claim transaction itself carry an RGB tapret commitment — i.e. the
//! claim *is* the witness transaction of the next RGB transition,
//! re-anchoring the swapped asset to a seal the claimer controls.

use anyhow::{Context, Result};
use rgbcore::bitcoin::hashes::{sha256, Hash};

/// Which branch of the HTLC a spend transaction exercises.
pub enum HtlcSpend<'a> {
    /// Hash branch: reveal the preimage, sign with the claimer key.
    Claim { preimage: &'a [u8] },
    /// Timeout branch: sign with the refund key after `csv_delay` blocks.
    Refund,
}

/// An optional extra output placed at vout 0 of the spend transaction
/// (used to carry an RGB tapret/opret commitment on the claim tx).
pub struct AnchorOut {
    pub spk: Vec<u8>,
    pub value_sat: u64,
}

/// Minimal script-number push (sufficient for CSV delays; positive
/// values only).
fn push_script_num(n: u32) -> Vec<u8> {
    assert!(n > 0, "CSV delay must be positive");
    if n <= 16 {
        return vec![0x50 + n as u8]; // OP_1..OP_16
    }
    let mut le = Vec::new();
    let mut v = n;
    while v > 0 {
        le.push((v & 0xff) as u8);
        v >>= 8;
    }
    // Script numbers are signed; add a zero byte if the top bit is set.
    if le.last().unwrap() & 0x80 != 0 {
        le.push(0x00);
    }
    let mut out = vec![le.len() as u8];
    out.extend_from_slice(&le);
    out
}

/// The full HTLC witness script (see module docs for the layout).
pub fn htlc_witness_script(
    hash: &[u8; 32],
    claimer_pk: &[u8; 33],
    refund_pk: &[u8; 33],
    csv_delay: u32,
) -> Vec<u8> {
    let mut s = Vec::with_capacity(112);
    s.push(0x63); // OP_IF
    s.push(0xa8); // OP_SHA256
    s.push(0x20); // push 32
    s.extend_from_slice(hash);
    s.push(0x88); // OP_EQUALVERIFY
    s.push(0x21); // push 33
    s.extend_from_slice(claimer_pk);
    s.push(0xac); // OP_CHECKSIG
    s.push(0x67); // OP_ELSE
    s.extend_from_slice(&push_script_num(csv_delay));
    s.push(0xb2); // OP_CHECKSEQUENCEVERIFY
    s.push(0x75); // OP_DROP
    s.push(0x21); // push 33
    s.extend_from_slice(refund_pk);
    s.push(0xac); // OP_CHECKSIG
    s.push(0x68); // OP_ENDIF
    s
}

/// P2WSH scriptPubKey for an arbitrary witness script.
pub fn p2wsh_spk(witness_script: &[u8]) -> Vec<u8> {
    let wsh = sha256::Hash::hash(witness_script);
    let mut spk = Vec::with_capacity(34);
    spk.push(0x00);
    spk.push(0x20);
    spk.extend_from_slice(wsh.as_byte_array());
    spk
}

/// Bech32 (v0) address for an arbitrary witness script.
pub fn p2wsh_address(network_hrp: &str, witness_script: &[u8]) -> Result<String> {
    use bech32::{hrp::Hrp, segwit};
    let wsh = sha256::Hash::hash(witness_script);
    let hrp = Hrp::parse(network_hrp).map_err(|e| anyhow::anyhow!("hrp: {e}"))?;
    let v0 = bech32::Fe32::try_from(0u8).unwrap();
    segwit::encode(hrp, v0, wsh.as_byte_array()).map_err(|e| anyhow::anyhow!("bech32 encode: {e}"))
}

/// Deterministic demo keypair from a label: `sk = SHA256(label)`.
/// Regtest-demo convenience only — obviously not a production key
/// derivation.
pub fn demo_keypair(label: &str) -> Result<(secp256k1::SecretKey, [u8; 33])> {
    let secp = secp256k1::Secp256k1::new();
    let sk_bytes = sha256::Hash::hash(label.as_bytes());
    let sk = secp256k1::SecretKey::from_slice(sk_bytes.as_byte_array())
        .context("label hashed to an invalid secret key (astronomically unlikely)")?;
    let pk = secp256k1::PublicKey::from_secret_key(&secp, &sk);
    Ok((sk, pk.serialize()))
}

/// Witness stack for an HTLC spend, given a finished signature.
fn htlc_witness_stack(sig_der_all: Vec<u8>, spend: &HtlcSpend, ws: &[u8]) -> Vec<Vec<u8>> {
    match spend {
        HtlcSpend::Claim { preimage } => vec![
            sig_der_all,
            preimage.to_vec(),
            vec![0x01], // minimal-true selects the IF branch
            ws.to_vec(),
        ],
        HtlcSpend::Refund => vec![
            sig_der_all,
            vec![], // empty selects the ELSE branch
            ws.to_vec(),
        ],
    }
}

/// Input sequence for the spend: refunds must encode the CSV delay
/// (BIP-68); claims just stay RBF-signalling.
fn htlc_sequence(spend: &HtlcSpend, csv_delay: u32) -> u32 {
    match spend {
        HtlcSpend::Claim { .. } => 0xffff_fffd,
        HtlcSpend::Refund => csv_delay,
    }
}

/// Build and sign the **Bitcoin** tx spending an HTLC UTXO.
///
/// Outputs: `[anchor?] + [dest]`. Fee is implicit
/// (`input − anchor − dest`).
#[allow(clippy::too_many_arguments)]
pub fn build_htlc_spend_btc(
    prev_txid: &str,
    prev_vout: u32,
    input_value_sat: u64,
    output_value_sat: u64,
    dest_spk: &[u8],
    witness_script: &[u8],
    spend: HtlcSpend,
    csv_delay: u32,
    signer_sk: &secp256k1::SecretKey,
    anchor: Option<AnchorOut>,
) -> Result<String> {
    use rgbcore::bitcoin::consensus::encode::serialize_hex;
    use rgbcore::bitcoin::sighash::{EcdsaSighashType, SighashCache};
    use rgbcore::bitcoin::{
        Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid, Witness,
    };

    let txid: Txid = prev_txid.parse().context("prev_txid")?;

    let input = TxIn {
        previous_output: OutPoint::new(txid, prev_vout),
        script_sig: ScriptBuf::new(),
        sequence: Sequence::from_consensus(htlc_sequence(&spend, csv_delay)),
        witness: Witness::new(),
    };

    let mut output = Vec::new();
    let anchor_value = anchor.as_ref().map(|a| a.value_sat).unwrap_or(0);
    if let Some(a) = &anchor {
        output.push(TxOut {
            value: Amount::from_sat(a.value_sat),
            script_pubkey: ScriptBuf::from_bytes(a.spk.clone()),
        });
    }
    output.push(TxOut {
        value: Amount::from_sat(output_value_sat),
        script_pubkey: ScriptBuf::from_bytes(dest_spk.to_vec()),
    });
    if output_value_sat + anchor_value >= input_value_sat {
        anyhow::bail!("outputs must total < input (need room for fee)");
    }

    let mut tx = Transaction {
        version: rgbcore::bitcoin::transaction::Version::TWO,
        lock_time: rgbcore::bitcoin::absolute::LockTime::ZERO,
        input: vec![input],
        output,
    };

    let sighash = SighashCache::new(&tx)
        .p2wsh_signature_hash(
            0,
            rgbcore::bitcoin::Script::from_bytes(witness_script),
            Amount::from_sat(input_value_sat),
            EcdsaSighashType::All,
        )
        .context("btc sighash")?;

    let secp = secp256k1::Secp256k1::new();
    let msg = secp256k1::Message::from_digest(sighash.to_byte_array());
    let mut sig = secp.sign_ecdsa(&msg, signer_sk).serialize_der().to_vec();
    sig.push(EcdsaSighashType::All as u8);

    for item in htlc_witness_stack(sig, &spend, witness_script) {
        tx.input[0].witness.push(&item);
    }
    Ok(serialize_hex(&tx))
}

/// Build and sign the **Elements/Liquid** tx spending an HTLC UTXO.
///
/// Outputs: `[anchor?] + [dest] + [explicit fee]`, all explicit
/// (unconfidential) — signing against a blinded HTLC input would need
/// the value commitment; the confidential variant is exercised by the
/// M4 transfer demo, not the swap.
#[allow(clippy::too_many_arguments)]
pub fn build_htlc_spend_liquid(
    prev_txid: &str,
    prev_vout: u32,
    input_value_sat: u64,
    output_value_sat: u64,
    fee_sat: u64,
    dest_spk: &[u8],
    lbtc_asset_hex: &str,
    witness_script: &[u8],
    spend: HtlcSpend,
    csv_delay: u32,
    signer_sk: &secp256k1::SecretKey,
    anchor: Option<AnchorOut>,
) -> Result<String> {
    use elements::confidential::{Asset, Nonce, Value};
    use elements::encode::serialize_hex;
    use elements::sighash::SighashCache;
    use elements::{
        AssetId, EcdsaSighashType, OutPoint, Script, Sequence, Transaction, TxIn, TxInWitness,
        TxOut, TxOutWitness, Txid,
    };

    let txid: Txid = prev_txid.parse().context("prev_txid")?;
    let asset_id: AssetId = lbtc_asset_hex.parse().context("lbtc asset id")?;
    let lbtc = Asset::Explicit(asset_id);

    let input = TxIn {
        previous_output: OutPoint::new(txid, prev_vout),
        is_pegin: false,
        script_sig: Script::new(),
        sequence: Sequence::from_consensus(htlc_sequence(&spend, csv_delay)),
        asset_issuance: Default::default(),
        witness: TxInWitness::default(),
    };

    let mut output = Vec::new();
    let anchor_value = anchor.as_ref().map(|a| a.value_sat).unwrap_or(0);
    if let Some(a) = &anchor {
        output.push(TxOut {
            asset: lbtc,
            value: Value::Explicit(a.value_sat),
            nonce: Nonce::Null,
            script_pubkey: Script::from(a.spk.clone()),
            witness: TxOutWitness::default(),
        });
    }
    output.push(TxOut {
        asset: lbtc,
        value: Value::Explicit(output_value_sat),
        nonce: Nonce::Null,
        script_pubkey: Script::from(dest_spk.to_vec()),
        witness: TxOutWitness::default(),
    });
    output.push(TxOut {
        asset: lbtc,
        value: Value::Explicit(fee_sat),
        nonce: Nonce::Null,
        script_pubkey: Script::new(),
        witness: TxOutWitness::default(),
    });
    if output_value_sat + anchor_value + fee_sat != input_value_sat {
        anyhow::bail!(
            "Elements outputs+fee must equal input exactly \
             ({output_value_sat} + {anchor_value} + {fee_sat} != {input_value_sat})"
        );
    }

    let mut tx = Transaction {
        version: 2,
        lock_time: elements::LockTime::ZERO,
        input: vec![input],
        output,
    };

    let sighash = SighashCache::new(&tx).segwitv0_sighash(
        0,
        &Script::from(witness_script.to_vec()),
        Value::Explicit(input_value_sat),
        EcdsaSighashType::All,
    );

    let secp = secp256k1::Secp256k1::new();
    let msg = secp256k1::Message::from_digest(sighash.to_byte_array());
    let mut sig = secp.sign_ecdsa(&msg, signer_sk).serialize_der().to_vec();
    sig.push(EcdsaSighashType::All as u8);

    tx.input[0].witness.script_witness = htlc_witness_stack(sig, &spend, witness_script);
    Ok(serialize_hex(&tx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swap::hash_of;

    fn demo_script() -> Vec<u8> {
        let h = hash_of(b"htlc-secret");
        let (_, claimer_pk) = demo_keypair("claimer").unwrap();
        let (_, refund_pk) = demo_keypair("refund").unwrap();
        htlc_witness_script(&h, &claimer_pk, &refund_pk, 5)
    }

    #[test]
    fn script_shape() {
        let ws = demo_script();
        assert_eq!(ws[0], 0x63, "OP_IF");
        assert_eq!(ws[1], 0xa8, "OP_SHA256");
        assert_eq!(ws[35], 0x88, "OP_EQUALVERIFY after 32-byte hash");
        assert_eq!(*ws.last().unwrap(), 0x68, "OP_ENDIF");
        // csv=5 encodes as OP_5
        assert!(ws.contains(&0x55));
    }

    #[test]
    fn csv_encoding() {
        assert_eq!(push_script_num(1), vec![0x51]);
        assert_eq!(push_script_num(16), vec![0x60]);
        assert_eq!(push_script_num(17), vec![0x01, 0x11]);
        assert_eq!(
            push_script_num(144),
            vec![0x02, 0x90, 0x00],
            "0x90 needs sign byte"
        );
        assert_eq!(push_script_num(600), vec![0x02, 0x58, 0x02]);
    }

    #[test]
    fn p2wsh_spk_shape() {
        let spk = p2wsh_spk(&demo_script());
        assert_eq!(spk.len(), 34);
        assert_eq!(spk[0], 0x00);
        assert_eq!(spk[1], 0x20);
    }

    #[test]
    fn same_program_both_hrps() {
        let ws = demo_script();
        let btc = p2wsh_address("bcrt", &ws).unwrap();
        let lq = p2wsh_address("ert", &ws).unwrap();
        let (_, _, bp) = bech32::segwit::decode(&btc).unwrap();
        let (_, _, lp) = bech32::segwit::decode(&lq).unwrap();
        assert_eq!(bp, lp);
    }

    #[test]
    fn demo_keys_are_deterministic_and_distinct() {
        let (sk1, pk1) = demo_keypair("alice").unwrap();
        let (sk2, pk2) = demo_keypair("alice").unwrap();
        let (_, pk3) = demo_keypair("bob").unwrap();
        assert_eq!(sk1.secret_bytes(), sk2.secret_bytes());
        assert_eq!(pk1, pk2);
        assert_ne!(pk1, pk3);
        assert_eq!(pk1.len(), 33);
    }

    #[test]
    fn btc_spend_builds_claim_and_refund() {
        let h = hash_of(b"htlc-secret");
        let (claim_sk, claimer_pk) = demo_keypair("claimer").unwrap();
        let (refund_sk, refund_pk) = demo_keypair("refund").unwrap();
        let ws = htlc_witness_script(&h, &claimer_pk, &refund_pk, 5);
        let dest = p2wsh_spk(&ws); // any spk works as a destination

        let claim = build_htlc_spend_btc(
            "1111111111111111111111111111111111111111111111111111111111111111",
            0,
            100_000,
            99_000,
            &dest,
            &ws,
            HtlcSpend::Claim {
                preimage: b"htlc-secret",
            },
            5,
            &claim_sk,
            None,
        )
        .unwrap();
        assert!(!claim.is_empty());

        let refund = build_htlc_spend_btc(
            "1111111111111111111111111111111111111111111111111111111111111111",
            0,
            100_000,
            99_000,
            &dest,
            &ws,
            HtlcSpend::Refund,
            5,
            &refund_sk,
            None,
        )
        .unwrap();
        assert!(!refund.is_empty());
        assert_ne!(claim, refund);
    }

    #[test]
    fn liquid_spend_requires_exact_balance() {
        let h = hash_of(b"htlc-secret");
        let (claim_sk, claimer_pk) = demo_keypair("claimer").unwrap();
        let (_, refund_pk) = demo_keypair("refund").unwrap();
        let ws = htlc_witness_script(&h, &claimer_pk, &refund_pk, 5);
        let dest = p2wsh_spk(&ws);
        let lbtc = "b2e15d0d7a0c94e4e2ce0fe6e8691b9e451377f6e46e8045a86f7c4b5d4f0f23";

        // 99_000 + 500 != 100_000 → must fail
        let bad = build_htlc_spend_liquid(
            "1111111111111111111111111111111111111111111111111111111111111111",
            0,
            100_000,
            99_000,
            500,
            &dest,
            lbtc,
            &ws,
            HtlcSpend::Claim {
                preimage: b"htlc-secret",
            },
            5,
            &claim_sk,
            None,
        );
        assert!(bad.is_err());

        let good = build_htlc_spend_liquid(
            "1111111111111111111111111111111111111111111111111111111111111111",
            0,
            100_000,
            99_500,
            500,
            &dest,
            lbtc,
            &ws,
            HtlcSpend::Claim {
                preimage: b"htlc-secret",
            },
            5,
            &claim_sk,
            None,
        );
        assert!(good.is_ok());
    }
}
