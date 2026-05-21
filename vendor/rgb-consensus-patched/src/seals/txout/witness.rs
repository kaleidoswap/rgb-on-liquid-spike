// Bitcoin protocol single-use-seals library.
//
// SPDX-License-Identifier: Apache-2.0
//
// Written in 2019-2024 by
//     Dr Maxim Orlovsky <orlovsky@lnp-bp.org>
//
// Copyright (C) 2019-2024 LNP/BP Standards Association. All rights reserved.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::marker::PhantomData;

use bitcoin::hashes::Hash;
use bitcoin::{Transaction as Tx, Txid};

use super::{TxoSeal, VerifyError};
use crate::commit_verify::mpc;
use crate::dbc;
use crate::single_use_seals::SealWitness;

/// Witness of a bitcoin-based seal being closed. Includes both transaction and
/// extra-transaction data.
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub struct Witness<D: dbc::Proof> {
    /// Witness transaction: transaction which contains commitment to the
    /// message over which the seal is closed.
    pub tx: Tx,

    /// Transaction id of the witness transaction above.
    pub txid: Txid,

    /// Deterministic bitcoin commitment proof from the anchor.
    pub proof: D,

    #[doc(hidden)]
    pub _phantom: PhantomData<D>,
}

impl<D: dbc::Proof> Witness<D> {
    /// Constructs witness from a witness transaction and extra-transaction
    /// proof, taken from an anchor.
    pub fn with(tx: Tx, dbc: D) -> Witness<D> {
        Witness {
            txid: tx.compute_txid(),
            tx,
            proof: dbc,
            _phantom: default!(),
        }
    }
}

impl<Seal: TxoSeal, Dbc: dbc::Proof> SealWitness<Seal> for Witness<Dbc> {
    type Message = mpc::Commitment;
    type Error = VerifyError<Dbc::Error>;

    fn verify_seal(&self, seal: &Seal, msg: &Self::Message) -> Result<(), Self::Error> {
        let outpoint = seal.outpoint().ok_or(VerifyError::NoWitnessTxid)?;
        verify_seal_against_witness(&self.tx, seal, msg, &self.proof, &[outpoint])
    }

    fn verify_many_seals<'seal>(
        &self,
        seals: impl IntoIterator<Item = &'seal Seal>,
        msg: &Self::Message,
    ) -> Result<(), Self::Error>
    where
        Seal: 'seal,
    {
        let outpoints: Vec<_> = seals
            .into_iter()
            .map(|seal| seal.outpoint().ok_or(VerifyError::NoWitnessTxid))
            .collect::<Result<_, _>>()?;
        verify_seal_against_witness(&self.tx, &(), msg, &self.proof, &outpoints)
    }
}

/// Generic seal-closure + DBC verification driven by [`WitnessTx`].
///
/// This is the shared helper that the bitcoin-flavored `Witness<D>`
/// delegates to, AND that any Liquid / Elements adapter would call. The
/// caller supplies a slice of outpoints that must all appear in
/// `tx.input_outpoints()`.
///
/// `_seal_marker` is only used for type-inference convenience; pass a
/// reference to the actual `Seal` (single-seal case) or `&()` (when
/// the seals were already concentrated into `outpoints`).
pub fn verify_seal_against_witness<W, Marker, Dbc>(
    tx: &W,
    _seal_marker: &Marker,
    msg: &mpc::Commitment,
    proof: &Dbc,
    outpoints: &[bitcoin::OutPoint],
) -> Result<(), VerifyError<Dbc::Error>>
where
    W: dbc::WitnessTx,
    Dbc: dbc::Proof,
{
    let tx_inputs = tx.input_outpoints();
    for outpoint in outpoints {
        let tup = (outpoint.txid.to_byte_array(), outpoint.vout);
        if !tx_inputs.iter().any(|i| *i == tup) {
            return Err(VerifyError::WitnessNotClosingSeal(*outpoint));
        }
    }
    proof.verify(msg, tx).map_err(VerifyError::Dbc)
}
