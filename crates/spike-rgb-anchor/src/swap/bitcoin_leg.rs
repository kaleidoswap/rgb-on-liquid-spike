//! Build the Bitcoin transaction that spends a hashlock P2WSH output
//! by revealing the preimage.
//!
//! The spend tx is 1-in / 1-out, witness = `[preimage, witnessScript]`.
//! No signature: the minimal hashlock has no key. The output pays the
//! claimer's address (full input value minus a flat fee).

use anyhow::{Context, Result};
use rgbcore::bitcoin::consensus::encode::serialize_hex;
use rgbcore::bitcoin::hashes::Hash;
use rgbcore::bitcoin::{
    Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid, Witness,
};

use super::hashlock_witness_script;

/// Build the raw (signed-equivalent) Bitcoin tx hex that spends the
/// hashlock UTXO `(prev_txid, prev_vout)` carrying `input_value_sat`,
/// paying `output_value_sat` to `dest_spk` (the destination
/// scriptPubKey bytes), revealing `preimage`.
pub fn build_hashlock_spend(
    prev_txid: &str,
    prev_vout: u32,
    input_value_sat: u64,
    output_value_sat: u64,
    dest_spk: &[u8],
    preimage: &[u8],
    hash: &[u8; 32],
) -> Result<String> {
    if output_value_sat >= input_value_sat {
        anyhow::bail!("output must be < input (need room for fee)");
    }
    let txid = Txid::from_slice(&{
        let mut b = hex::decode(prev_txid).context("prev_txid hex")?;
        if b.len() != 32 {
            anyhow::bail!("prev_txid must be 32 bytes");
        }
        b.reverse(); // RPC txids are display (big-endian); internal is LE
        b
    })
    .map_err(|e| anyhow::anyhow!("txid: {e}"))?;

    let witness_script = hashlock_witness_script(hash);

    let mut input = TxIn {
        previous_output: OutPoint::new(txid, prev_vout),
        script_sig: ScriptBuf::new(),
        sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
        witness: Witness::new(),
    };
    // Witness stack: [ preimage, witnessScript ]
    input.witness.push(preimage);
    input.witness.push(&witness_script);

    let output = TxOut {
        value: Amount::from_sat(output_value_sat),
        script_pubkey: ScriptBuf::from_bytes(dest_spk.to_vec()),
    };

    let tx = Transaction {
        version: rgbcore::bitcoin::transaction::Version::TWO,
        lock_time: rgbcore::bitcoin::absolute::LockTime::ZERO,
        input: vec![input],
        output: vec![output],
    };
    let _ = input_value_sat; // documented in the fee comment above
    Ok(serialize_hex(&tx))
}
