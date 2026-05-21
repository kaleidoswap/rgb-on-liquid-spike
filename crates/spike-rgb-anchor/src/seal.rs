//! Liquid-flavored RGB seal closing.
//!
//! In RGB, a "seal" is a UTXO. The seal is "closed" by the transaction
//! that spends it; the same transaction MUST embed the anchor
//! commitment (the tapret MPC root) in one of its outputs.
//!
//! `rgbcore`'s `TxoSeal` and `Anchor` machinery is typed against
//! rust-bitcoin's `Tx` / `Txid`. This module models the seal-closure
//! verification step directly against an Elements transaction fetched
//! from `elementsd` over JSON-RPC.
//!
//! Verification checks:
//!   1. The witness tx's input list contains the seal outpoint
//!      (the seal was closed).
//!   2. One of the witness tx's outputs has the scriptPubKey produced
//!      by `liquid_dbc::commit(internal_pk, mpc_root)`
//!      (closing the seal committed to `mpc_root`).
//!   3. The same scriptPubKey verifies under `liquid_dbc::verify`
//!      (rgb-consensus's own `ConvolveCommitProof`).
//!
//! Check 3 is redundant given check 2, but it is performed by
//! unmodified upstream code, which is the part the proposed patch
//! generalizes.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiquidSeal {
    pub txid: String,
    pub vout: u32,
}

#[derive(Debug)]
pub struct WitnessTx {
    pub txid: String,
    pub vin_outpoints: Vec<(String, u32)>,
    pub vouts_spk_hex: Vec<String>,
}

/// `rgbcore::dbc::WitnessTx` impl — lets us hand a Liquid tx straight to
/// `rgb-consensus-patched::dbc::Anchor::verify`. This is the line of
/// code that, with the upstream patch landed, would be the entire
/// integration point for an Elements/Liquid backend.
impl rgbcore::dbc::WitnessTx for WitnessTx {
    fn witness_txid(&self) -> [u8; 32] {
        let mut out = [0u8; 32];
        if let Ok(b) = hex::decode(&self.txid) {
            if b.len() == 32 {
                out.copy_from_slice(&b);
            }
        }
        out
    }

    fn input_outpoints(&self) -> Vec<([u8; 32], u32)> {
        self.vin_outpoints
            .iter()
            .map(|(txid, vout)| {
                let mut out = [0u8; 32];
                if let Ok(b) = hex::decode(txid) {
                    if b.len() == 32 {
                        out.copy_from_slice(&b);
                    }
                }
                (out, *vout)
            })
            .collect()
    }

    fn output_script_pubkeys(&self) -> Vec<Vec<u8>> {
        self.vouts_spk_hex
            .iter()
            .map(|h| hex::decode(h).unwrap_or_default())
            .collect()
    }
}

pub async fn fetch_witness_tx(
    rpc: &spike_env::elements_rpc::ElementsRpc,
    txid: &str,
) -> Result<WitnessTx> {
    let v = rpc
        .call("getrawtransaction", serde_json::json!([txid, 2]))
        .await
        .context("getrawtransaction")?;

    let vin_outpoints = v["vin"]
        .as_array()
        .context("no vin array")?
        .iter()
        .map(|x| {
            let txid = x["txid"].as_str().unwrap_or_default().to_lowercase();
            let vout = x["vout"].as_u64().unwrap_or_default() as u32;
            (txid, vout)
        })
        .collect();

    let vouts_spk_hex = v["vout"]
        .as_array()
        .context("no vout array")?
        .iter()
        .map(|x| {
            x["scriptPubKey"]["hex"]
                .as_str()
                .unwrap_or_default()
                .to_lowercase()
        })
        .collect();

    Ok(WitnessTx {
        txid: txid.to_owned(),
        vin_outpoints,
        vouts_spk_hex,
    })
}

#[derive(Debug)]
pub struct SealVerification {
    pub seal_input_index: usize,
    pub commitment_output_index: usize,
}

/// Verify that `witness_tx`:
///   (a) spends `seal` (i.e. (seal.txid, seal.vout) appears in vin), and
///   (b) has an output whose scriptPubKey hex equals `expected_spk_hex`.
pub fn verify_seal_closure(
    witness_tx: &WitnessTx,
    seal: &LiquidSeal,
    expected_spk_hex: &str,
) -> Result<SealVerification> {
    let seal_txid = seal.txid.to_lowercase();
    let seal_input_index = witness_tx
        .vin_outpoints
        .iter()
        .position(|(txid, vout)| txid == &seal_txid && *vout == seal.vout)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "seal {}:{} is NOT among the {} inputs of witness tx {} — seal not closed by this tx",
                seal.txid, seal.vout, witness_tx.vin_outpoints.len(), witness_tx.txid
            )
        })?;

    let expected = expected_spk_hex.to_lowercase();
    let commitment_output_index = witness_tx
        .vouts_spk_hex
        .iter()
        .position(|spk| spk == &expected)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no output of witness tx {} carries the expected tapret scriptPubKey ({}); \
                 scanned {} outputs",
                witness_tx.txid,
                expected_spk_hex,
                witness_tx.vouts_spk_hex.len()
            )
        })?;

    Ok(SealVerification {
        seal_input_index,
        commitment_output_index,
    })
}
