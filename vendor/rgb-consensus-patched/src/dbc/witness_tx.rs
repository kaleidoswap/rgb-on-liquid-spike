// Deterministic bitcoin commitments library.
//
// SPDX-License-Identifier: Apache-2.0
//
// Chain-agnostic witness-transaction abstraction.
//
// Added by the RGB-on-Liquid RFC patch. See docs/RFC_RGB_ON_LIQUID.md
// in the kaleidoswap/rgb-on-liquid-spike repo for the design rationale.

use bitcoin::hashes::Hash;
use bitcoin::Transaction;

/// A confirmed transaction, viewed only through the lens that RGB seal /
/// dbc verification needs:
///
///   - its txid,
///   - the outpoints it spends (for seal-closure checks),
///   - the raw script_pubkey bytes of each output (for tapret/opret
///     commitment recovery).
///
/// `bitcoin::Transaction` implements this trait directly. Any other
/// transaction type — for example an Elements/Liquid transaction, a
/// JSON-RPC `getrawtransaction` response, or a wallet-side bespoke
/// type — can implement it to participate in the same verification
/// paths.
///
/// The trait deliberately does NOT expose witness data, signatures,
/// fee math, version, or locktime. None of those are needed for
/// commitment / seal-closure verification, and keeping them out makes
/// the trait trivially implementable for thin tx adapters.
pub trait WitnessTx {
    /// 32-byte witness identifier (txid).
    fn witness_txid(&self) -> [u8; 32];

    /// The outpoints this transaction spends, in input order.
    fn input_outpoints(&self) -> Vec<([u8; 32], u32)>;

    /// The script_pubkey bytes of each output, in output order.
    fn output_script_pubkeys(&self) -> Vec<Vec<u8>>;
}

impl WitnessTx for Transaction {
    fn witness_txid(&self) -> [u8; 32] {
        self.compute_txid().to_byte_array()
    }

    fn input_outpoints(&self) -> Vec<([u8; 32], u32)> {
        self.input
            .iter()
            .map(|i| (i.previous_output.txid.to_byte_array(), i.previous_output.vout))
            .collect()
    }

    fn output_script_pubkeys(&self) -> Vec<Vec<u8>> {
        self.output
            .iter()
            .map(|o| o.script_pubkey.to_bytes())
            .collect()
    }
}
