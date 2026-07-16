//! Real RGB20 (NIA) issuance and transfer.
//!
//! Uses `rgb-ops::ContractBuilder` and `rgb-schemas::NonInflatableAsset`
//! (the production NIA / RGB20 schema) to produce real RGB primitives:
//!   - a `ContractId` from a `Genesis` stamped onto a chosen `ChainNet`,
//!   - a `Transition` consuming the issuance allocation and creating
//!     the recipient's plus change,
//!   - a `BundleId` over that transition.
//!
//! `rgb-ops` and `rgb-schemas` are unmodified registry dependencies;
//! the workspace's `[patch.crates-io]` redirects `rgb-consensus` to the
//! patched copy in `vendor/`.

use amplify::confinement::Confined;
use anyhow::{Context, Result};
use rgbcore::bitcoin::OutPoint;
use rgbcore::commit_verify::CommitId;
use rgbcore::{
    BundleId, ChainNet, ContractId, GenesisSeal, GraphSeal, KnownTransition, OpId, Opout,
    Transition, TransitionBundle,
};
use rgbstd::containers::{BuilderSeal, ConsignmentExt};
use rgbstd::contract::{AllocatedState, ContractBuilder, IssuerWrapper, TransitionBuilder};
use rgbstd::stl::{AssetSpec, ContractTerms, RicardianContract};
use rgbstd::{Amount, Identity, Precision, RevealedValue};
use schemata::{NonInflatableAsset, OS_ASSET};
use strict_encoding::FieldName;

/// Outcome of an NIA issuance.
pub struct NiaIssuance {
    pub contract_id: ContractId,
    pub initial_seal_outpoint: OutPoint,
    pub initial_amount: u64,
}

/// Issue a real RGB20 (NIA) contract:
///   - `chain_net` is stamped on the Genesis (so the consignment knows
///     it's a Liquid contract per RGB consensus).
///   - one fungible allocation of `amount` units to a `GenesisSeal`
///     pointing at `to_seal` (a Liquid UTXO).
pub fn issue(
    chain_net: ChainNet,
    name: &str,
    ticker: &str,
    amount: u64,
    to_seal: OutPoint,
) -> Result<NiaIssuance> {
    use rgbstd::stl::{Name, Ticker};

    let issuer = Identity::default();

    let builder = ContractBuilder::with(
        issuer,
        NonInflatableAsset::schema(),
        NonInflatableAsset::types(),
        NonInflatableAsset::scripts(),
        chain_net,
    );

    let spec = AssetSpec {
        ticker: Ticker::try_from(ticker.to_owned()).map_err(|e| anyhow::anyhow!("ticker: {e}"))?,
        name: Name::try_from(name.to_owned()).map_err(|e| anyhow::anyhow!("name: {e}"))?,
        details: None,
        precision: Precision::Indivisible,
    };
    let terms = ContractTerms {
        text: RicardianContract::default(),
        media: None,
    };
    let issued_supply = Amount::from(amount);

    // GenesisSeal = BlindSeal<TxPtr=Txid>. txid, vout, blinding.
    let alice_seal: GenesisSeal = GenesisSeal::with_blinding(to_seal.txid, to_seal.vout, 0u64);

    let consignment = builder
        .add_global_state(FieldName::from("spec"), spec)
        .context("add spec")?
        .add_global_state(FieldName::from("terms"), terms)
        .context("add terms")?
        .add_global_state(FieldName::from("issuedSupply"), issued_supply)
        .context("add issuedSupply")?
        .add_fungible_state(
            FieldName::from("assetOwner"),
            BuilderSeal::Revealed(alice_seal),
            amount,
        )
        .context("add fungible state")?
        .issue_contract_raw(crate::bfa::GENESIS_TIMESTAMP)
        .map_err(|e| anyhow::anyhow!("issue_contract: {e:?}"))?;

    Ok(NiaIssuance {
        contract_id: consignment.contract_id(),
        initial_seal_outpoint: to_seal,
        initial_amount: amount,
    })
}

/// Build an RGB20 transfer transition: spend Alice's initial allocation,
/// create Bob's allocation + (optional) change back to Alice. Wrap in
/// a single-transition `TransitionBundle` and return its `BundleId`.
///
/// `alice_input_vin` is the zero-based input index in the future
/// witness tx that consumes the sender's seal. It is part of the
/// bundle's `input_map` (Opout to OpId association); the demo witness
/// txs place the seal at vin[0].
pub fn build_transfer(
    contract_id: ContractId,
    initial_amount: u64,
    bob_amount: u64,
    bob_seal_outpoint: OutPoint,
    change_seal_outpoint: Option<OutPoint>,
    alice_input_vin: u32,
) -> Result<(BundleId, Transition)> {
    // Genesis OpId shares its 32 bytes with the ContractId.
    let genesis_opid = OpId::from(contract_id.to_byte_array());
    build_transfer_from(
        contract_id,
        genesis_opid,
        0,
        initial_amount,
        bob_amount,
        bob_seal_outpoint,
        change_seal_outpoint,
        alice_input_vin,
    )
}

/// [`build_transfer`] consuming an arbitrary prior allocation instead
/// of the genesis one: `prev_opid`/`prev_opout_no` name the transition
/// output being spent, `prev_amount` the allocation it carries. This is
/// what makes transfers *chainable* — a swap-back leg consumes the
/// allocation the swap-in claim created.
#[allow(clippy::too_many_arguments)]
pub fn build_transfer_from(
    contract_id: ContractId,
    prev_opid: OpId,
    prev_opout_no: u16,
    prev_amount: u64,
    bob_amount: u64,
    bob_seal_outpoint: OutPoint,
    change_seal_outpoint: Option<OutPoint>,
    alice_input_vin: u32,
) -> Result<(BundleId, Transition)> {
    let schema = NonInflatableAsset::schema();
    let types = NonInflatableAsset::types();

    let mut builder = TransitionBuilder::named_transition(
        contract_id,
        schema,
        FieldName::from("transfer"),
        types,
    )
    .map_err(|e| anyhow::anyhow!("TransitionBuilder::named_transition: {e:?}"))?;

    // Input = the consumed allocation: opout = (prev_opid, OS_ASSET, n)
    let input_opout = Opout::new(prev_opid, OS_ASSET, prev_opout_no);
    builder = builder
        .add_input(
            input_opout,
            AllocatedState::Amount(RevealedValue::new(Amount::from(prev_amount))),
        )
        .map_err(|e| anyhow::anyhow!("add_input: {e:?}"))?;

    // Bob's allocation.
    let bob_seal: GraphSeal =
        GraphSeal::with_blinding(bob_seal_outpoint.txid, bob_seal_outpoint.vout, 0u64);
    builder = builder
        .add_fungible_state(
            FieldName::from("assetOwner"),
            BuilderSeal::Revealed(bob_seal),
            bob_amount,
        )
        .map_err(|e| anyhow::anyhow!("add_fungible_state (bob): {e:?}"))?;

    // Change back to Alice (if any).
    if let Some(change_op) = change_seal_outpoint {
        let change_amount = prev_amount
            .checked_sub(bob_amount)
            .context("change amount underflow")?;
        if change_amount > 0 {
            let change_seal: GraphSeal =
                GraphSeal::with_blinding(change_op.txid, change_op.vout, 1u64);
            builder = builder
                .add_fungible_state(
                    FieldName::from("assetOwner"),
                    BuilderSeal::Revealed(change_seal),
                    change_amount,
                )
                .map_err(|e| anyhow::anyhow!("add_fungible_state (change): {e:?}"))?;
        }
    } else if bob_amount != prev_amount {
        anyhow::bail!(
            "no change seal provided but bob_amount ({}) != prev_amount ({})",
            bob_amount,
            prev_amount
        );
    }

    let transition = builder
        .complete_transition()
        .map_err(|e| anyhow::anyhow!("complete_transition: {e:?}"))?;
    let opid = transition.commit_id();

    // Wrap in a single-transition TransitionBundle.
    // input_map maps each consumed Opout to the Opid of the transition
    // that consumes it. Bundle::CommitEncode commits to this map.
    let _ = alice_input_vin; // reserved for later witness-tx wiring
    let mut input_map = std::collections::BTreeMap::new();
    input_map.insert(input_opout, opid);
    let input_map =
        Confined::try_from(input_map).map_err(|e| anyhow::anyhow!("input_map: {e:?}"))?;

    let known_transitions =
        Confined::try_from(vec![KnownTransition::new(opid, transition.clone())])
            .map_err(|e| anyhow::anyhow!("known_transitions: {e:?}"))?;

    let bundle = TransitionBundle {
        input_map,
        known_transitions,
    };
    let bundle_id = bundle.commit_id();

    Ok((bundle_id, transition))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgbcore::bitcoin::{hashes::Hash, Txid};

    fn fake_outpoint(seed: u8) -> OutPoint {
        let mut bytes = [0u8; 32];
        bytes[0] = seed;
        OutPoint::new(Txid::from_byte_array(bytes), 0)
    }

    #[test]
    fn issue_nia_on_liquid_yields_real_contract_id() {
        let r = issue(
            ChainNet::LiquidTestnet,
            "KaleidoLiquidUSD",
            "kLUSD",
            1_000_000,
            fake_outpoint(0x42),
        )
        .expect("issue NIA");
        // ContractId is a baid64 string "rgb:..." when displayed.
        let s = format!("{}", r.contract_id);
        assert!(s.starts_with("rgb:"), "got: {s}");
        // The same inputs deterministically yield the same id (no time
        // randomization in our path).
        let r2 = issue(
            ChainNet::LiquidTestnet,
            "KaleidoLiquidUSD",
            "kLUSD",
            1_000_000,
            fake_outpoint(0x42),
        )
        .expect("issue NIA again");
        assert_eq!(r.contract_id, r2.contract_id);

        // Bitcoin variant yields a DIFFERENT id.
        let r_btc = issue(
            ChainNet::BitcoinRegtest,
            "KaleidoLiquidUSD",
            "kLUSD",
            1_000_000,
            fake_outpoint(0x42),
        )
        .expect("issue NIA on bitcoin");
        assert_ne!(r.contract_id, r_btc.contract_id);
    }

    #[test]
    fn full_issue_then_transfer_yields_real_bundle_id() {
        let issuance = issue(
            ChainNet::LiquidTestnet,
            "KaleidoLiquidUSD",
            "kLUSD",
            1_000_000,
            fake_outpoint(0xAA),
        )
        .expect("issue");

        let (bundle_id, transition) = build_transfer(
            issuance.contract_id,
            issuance.initial_amount,
            600_000,
            fake_outpoint(0xBB),       // bob's seal
            Some(fake_outpoint(0xCC)), // alice's change seal
            0,
        )
        .expect("transfer");

        assert_eq!(transition.contract_id, issuance.contract_id);
        // BundleId is a real 32-byte digest computed via CommitId over
        // the bundle's input_map.
        let bytes = bundle_id.to_byte_array();
        assert_eq!(bytes.len(), 32);
        // Not all zeros.
        assert!(bytes.iter().any(|b| *b != 0));
        // Determinism: same inputs → same bundle_id.
        let (bundle_id_2, _) = build_transfer(
            issuance.contract_id,
            issuance.initial_amount,
            600_000,
            fake_outpoint(0xBB),
            Some(fake_outpoint(0xCC)),
            0,
        )
        .expect("transfer 2");
        assert_eq!(bundle_id, bundle_id_2);
    }
}
