//! Build the Elements (Liquid) transaction that spends a hashlock
//! P2WSH output by revealing the preimage.
//!
//! Elements txs differ from Bitcoin: every output carries an explicit
//! (or blinded) asset + value + nonce, and there is an explicit fee
//! output with an empty scriptPubKey. These spend txs use fully
//! explicit (unconfidential) outputs.

use anyhow::{Context, Result};
use elements::confidential::{Asset, Nonce, Value};
use elements::encode::serialize_hex;
use elements::hashes::Hash as _;
use elements::{
    AssetId, OutPoint, Script, Sequence, Transaction, TxIn, TxInWitness, TxOut, TxOutWitness, Txid,
};

use super::hashlock_witness_script;

/// Build the raw Elements tx hex that spends the hashlock UTXO
/// `(prev_txid, prev_vout)`, paying `output_value_sat` of `lbtc_asset`
/// to `dest_spk`, with `fee_sat` as the explicit fee, revealing
/// `preimage`.
#[allow(clippy::too_many_arguments)]
pub fn build_hashlock_spend(
    prev_txid: &str,
    prev_vout: u32,
    output_value_sat: u64,
    fee_sat: u64,
    dest_spk: &[u8],
    lbtc_asset_hex: &str,
    preimage: &[u8],
    hash: &[u8; 32],
) -> Result<String> {
    let txid: Txid = prev_txid.parse().context("prev_txid")?;

    // L-BTC asset id. RPC gives it display-order; AssetId::from_str
    // handles that.
    let asset_id: AssetId = lbtc_asset_hex.parse().context("lbtc asset id")?;
    let lbtc = Asset::Explicit(asset_id);

    let witness_script = hashlock_witness_script(hash);

    let input = TxIn {
        previous_output: OutPoint::new(txid, prev_vout),
        is_pegin: false,
        script_sig: Script::new(),
        sequence: Sequence::from_consensus(0xffff_fffd),
        asset_issuance: Default::default(),
        witness: TxInWitness {
            amount_rangeproof: None,
            inflation_keys_rangeproof: None,
            script_witness: vec![preimage.to_vec(), witness_script],
            pegin_witness: vec![],
        },
    };

    let dest_out = TxOut {
        asset: lbtc,
        value: Value::Explicit(output_value_sat),
        nonce: Nonce::Null,
        script_pubkey: Script::from(dest_spk.to_vec()),
        witness: TxOutWitness::default(),
    };

    // Elements explicit fee output: empty scriptPubKey.
    let fee_out = TxOut {
        asset: lbtc,
        value: Value::Explicit(fee_sat),
        nonce: Nonce::Null,
        script_pubkey: Script::new(),
        witness: TxOutWitness::default(),
    };

    let tx = Transaction {
        version: 2,
        lock_time: elements::LockTime::ZERO,
        input: vec![input],
        output: vec![dest_out, fee_out],
    };
    let _ = Txid::all_zeros; // keep the Hash import referenced
    Ok(serialize_hex(&tx))
}
