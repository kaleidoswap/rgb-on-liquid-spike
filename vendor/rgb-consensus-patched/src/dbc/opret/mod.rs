// Deterministic bitcoin commitments library.
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

//! ScriptPubkey-based OP_RETURN commitments.

mod tx;
mod txout;
mod spk;

use bitcoin::ScriptBuf;
use strict_encoding::{DefaultBasedStrictDumb, StrictDeserialize, StrictSerialize};

use crate::commit_verify::mpc::Commitment;
use crate::commit_verify::{CommitmentProtocol, EmbedCommitVerify, EmbedVerifyError};
use crate::dbc::proof::Method;
use crate::dbc::Proof;
use crate::LIB_NAME_BPCORE;

/// Marker non-instantiable enum defining LNPBP-12 taproot OP_RETURN (`tapret`)
/// protocol.
pub enum OpretFirst {}

impl CommitmentProtocol for OpretFirst {}

/// Errors during tapret commitment.
#[derive(Clone, Eq, PartialEq, Debug, Display, Error, From)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate", rename_all = "camelCase")
)]
#[display(doc_comments)]
pub enum OpretError {
    /// transaction doesn't contain OP_RETURN output.
    NoOpretOutput,

    /// first OP_RETURN output inside the transaction already contains some
    /// data.
    InvalidOpretScript,
}

/// Empty type for use inside [`crate::Anchor`] for opret commitment scheme.
#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug, Default)]
#[derive(StrictType, StrictEncode, StrictDecode)]
#[strict_type(lib = LIB_NAME_BPCORE)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate", rename_all = "camelCase")
)]
pub struct OpretProof(());

impl DefaultBasedStrictDumb for OpretProof {}
impl StrictSerialize for OpretProof {}
impl StrictDeserialize for OpretProof {}

impl Proof<Method> for OpretProof {
    type Error = EmbedVerifyError<OpretError>;

    fn method(&self) -> Method {
        Method::OpretFirst
    }

    /// Verifies that `tx` contains an OP_RETURN output committing to
    /// `msg` under this proof.
    ///
    /// We scan the tx's outputs for the first OP_RETURN and run the
    /// SPK-level `EmbedCommitVerify` against its bytes. This is exactly
    /// what the previous `&Tx`-flavored path did internally, minus the
    /// surrounding tx envelope — which makes it work identically
    /// against Elements/Liquid OP_RETURN outputs.
    fn verify<W: crate::dbc::WitnessTx>(
        &self,
        msg: &Commitment,
        tx: &W,
    ) -> Result<(), EmbedVerifyError<OpretError>> {
        for spk_bytes in tx.output_script_pubkeys() {
            let spk = ScriptBuf::from_bytes(spk_bytes);
            if !spk.is_op_return() {
                continue;
            }
            return <ScriptBuf as EmbedCommitVerify<Commitment, OpretFirst>>::verify(
                &spk, msg, self,
            );
        }
        Err(EmbedVerifyError::InvalidMessage(OpretError::NoOpretOutput))
    }
}
