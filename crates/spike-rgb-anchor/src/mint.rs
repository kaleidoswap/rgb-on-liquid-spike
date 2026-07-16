//! Backed minting: IFA (Inflatable Fungible Asset) issuance and mint
//! transitions whose witness transaction locks a native Elements asset.
//!
//! This is the "LR-USDT" construction. The contract is an IFA whose
//! entire supply starts as an **inflation allowance** assigned to a
//! *gate seal* (a Liquid UTXO). Minting is a `TS_INFLATION` transition:
//! it consumes the gate seal, issues new units to the recipient, and
//! re-assigns the remaining allowance to a fresh gate seal.
//!
//! The backing rule lives in the witness transaction: the same Liquid
//! tx that closes the gate seal and carries the mint's anchor must also
//! **lock the backing asset** (e.g. native USDt) into a vault output,
//! one backing unit per minted unit. Because every RGB validator
//! already fetches the witness transaction to check the anchor, and
//! because Elements outputs name their asset explicitly, any holder can
//! verify the backing of every mint in their history with no oracle:
//! see [`verify_backing`].
//!
//! IFA's own AluVM validators enforce the supply arithmetic
//! client-side: issued amounts must match assignments, and a mint
//! consuming an allowance of N can issue at most N. The schema is used
//! unmodified from `rgb-schemas`.

use amplify::confinement::Confined;
use anyhow::{Context, Result};
use rgbcore::bitcoin::OutPoint;
use rgbcore::commit_verify::CommitId;
use rgbcore::{
    BundleId, ChainNet, ContractId, GenesisSeal, GraphSeal, KnownTransition, OpId, Opout,
    Transition, TransitionBundle,
};
use rgbstd::containers::ConsignmentExt;
use rgbstd::contract::{AllocatedState, ContractBuilder, IssuerWrapper, TransitionBuilder};
use rgbstd::stl::{AssetSpec, ContractTerms, RicardianContract};
use rgbstd::{Amount, Identity, Precision, RevealedValue};
use schemata::{InflatableFungibleAsset, OS_INFLATION};
use strict_encoding::FieldName;

/// Outcome of an IFA issuance: all supply starts as inflation
/// allowance on the gate seal; nothing is issued yet.
pub struct IfaIssuance {
    pub contract_id: ContractId,
    pub gate_seal_outpoint: OutPoint,
    pub max_supply: u64,
}

/// Issue an IFA contract with zero circulating supply and the full
/// `max_supply` as an inflation allowance assigned to `gate_seal`.
pub fn issue_ifa(
    chain_net: ChainNet,
    name: &str,
    ticker: &str,
    max_supply: u64,
    gate_seal: OutPoint,
) -> Result<IfaIssuance> {
    use rgbstd::stl::{Name, Ticker};

    let issuer = Identity::default();

    let builder = ContractBuilder::with(
        issuer,
        InflatableFungibleAsset::schema(),
        InflatableFungibleAsset::types(),
        InflatableFungibleAsset::scripts(),
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

    let gate: GenesisSeal = GenesisSeal::with_blinding(gate_seal.txid, gate_seal.vout, 0u64);

    let consignment = builder
        .add_global_state(FieldName::from("spec"), spec)
        .context("add spec")?
        .add_global_state(FieldName::from("terms"), terms)
        .context("add terms")?
        .add_global_state(FieldName::from("issuedSupply"), Amount::from(0u64))
        .context("add issuedSupply")?
        .add_global_state(FieldName::from("maxSupply"), Amount::from(max_supply))
        .context("add maxSupply")?
        .add_fungible_state(FieldName::from("inflationAllowance"), gate, max_supply)
        .context("add inflationAllowance")?
        .issue_contract()
        .map_err(|e| anyhow::anyhow!("issue_contract: {e:?}"))?;

    Ok(IfaIssuance {
        contract_id: consignment.contract_id(),
        gate_seal_outpoint: gate_seal,
        max_supply,
    })
}

/// Build a mint (`TS_INFLATION`) transition:
///   input: the gate seal carrying `allowance_before` inflation rights
///           (`gate_opout_no` selects which assignment index; 0 for a
///           freshly issued contract),
///   outputs: `mint_amount` new units to `recipient_seal`, and the
///           remaining allowance to `new_gate_seal` (required unless
///           the allowance is exhausted by this mint).
///
/// IFA's AluVM validator enforces: issuedSupply(op) = sum of new asset
/// allocations, allowance metadata = sum of re-assigned rights, and
/// input rights = issued + remaining.
pub fn build_mint(
    contract_id: ContractId,
    gate_opid: OpId,
    gate_opout_no: u16,
    allowance_before: u64,
    mint_amount: u64,
    recipient_seal: OutPoint,
    new_gate_seal: Option<OutPoint>,
) -> Result<(BundleId, Transition)> {
    let remaining = allowance_before
        .checked_sub(mint_amount)
        .context("mint exceeds allowance")?;

    let schema = InflatableFungibleAsset::schema();
    let types = InflatableFungibleAsset::types();

    let mut builder =
        TransitionBuilder::named_transition(contract_id, schema, FieldName::from("inflate"), types)
            .map_err(|e| anyhow::anyhow!("TransitionBuilder::named_transition: {e:?}"))?;

    // Consume the gate seal's inflation allowance.
    let input_opout = Opout::new(gate_opid, OS_INFLATION, gate_opout_no);
    builder = builder
        .add_input(
            input_opout,
            AllocatedState::Amount(RevealedValue::new(Amount::from(allowance_before))),
        )
        .map_err(|e| anyhow::anyhow!("add_input: {e:?}"))?;

    // This operation's issued supply and the post-mint allowance,
    // checked by the schema validator against the sums below.
    builder = builder
        .add_global_state(FieldName::from("issuedSupply"), Amount::from(mint_amount))
        .map_err(|e| anyhow::anyhow!("add issuedSupply: {e:?}"))?
        .add_metadata(FieldName::from("allowedInflation"), Amount::from(remaining))
        .map_err(|e| anyhow::anyhow!("add allowedInflation: {e:?}"))?;

    // Newly minted units to the recipient.
    let recipient: GraphSeal =
        GraphSeal::with_blinding(recipient_seal.txid, recipient_seal.vout, 0u64);
    builder = builder
        .add_fungible_state(FieldName::from("assetOwner"), recipient, mint_amount)
        .map_err(|e| anyhow::anyhow!("add_fungible_state (recipient): {e:?}"))?;

    // Remaining allowance rolls to the next gate seal.
    if remaining > 0 {
        let gate = new_gate_seal.context("remaining allowance > 0 requires a new gate seal")?;
        let gate: GraphSeal = GraphSeal::with_blinding(gate.txid, gate.vout, 1u64);
        builder = builder
            .add_fungible_state(FieldName::from("inflationAllowance"), gate, remaining)
            .map_err(|e| anyhow::anyhow!("add_fungible_state (gate): {e:?}"))?;
    }

    let transition = builder
        .complete_transition()
        .map_err(|e| anyhow::anyhow!("complete_transition: {e:?}"))?;
    let opid = transition.commit_id();

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
    Ok((bundle.commit_id(), transition))
}

/// The backing check every holder runs: does `witness_tx` (the Liquid
/// transaction that anchors the mint) lock at least `required_amount`
/// of `backing_asset` into the `vault_spk` output?
///
/// Elements outputs name their asset explicitly, so this is a direct
/// read of the transaction: no oracle, no attestation. Blinded vault
/// outputs are rejected: a peg vault must be publicly auditable.
pub fn verify_backing(
    witness_tx: &elements::Transaction,
    vault_spk: &[u8],
    backing_asset: &elements::AssetId,
    required_amount: u64,
) -> Result<u64> {
    use elements::confidential::{Asset, Value};

    let mut locked = 0u64;
    for out in &witness_tx.output {
        if out.script_pubkey.as_bytes() != vault_spk {
            continue;
        }
        match (&out.asset, &out.value) {
            (Asset::Explicit(id), Value::Explicit(v)) if id == backing_asset => {
                locked = locked.saturating_add(*v);
            }
            (Asset::Explicit(_), _) | (_, Value::Explicit(_)) => {
                anyhow::bail!("vault output is partially blinded; the vault must be explicit");
            }
            _ => anyhow::bail!("vault output is blinded; the vault must be explicit"),
        }
    }

    if locked < required_amount {
        anyhow::bail!(
            "under-backed mint: locked {locked} of backing asset, mint requires {required_amount}"
        );
    }
    Ok(locked)
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
    fn issue_ifa_yields_real_contract_id() {
        let r = issue_ifa(
            ChainNet::LiquidTestnet,
            "LiquidRgbUSD",
            "LRUSD",
            1_000_000,
            fake_outpoint(0x11),
        )
        .expect("issue IFA");
        let s = format!("{}", r.contract_id);
        assert!(s.starts_with("rgb:"), "got: {s}");
        // Different chain, different contract.
        let r_btc = issue_ifa(
            ChainNet::BitcoinRegtest,
            "LiquidRgbUSD",
            "LRUSD",
            1_000_000,
            fake_outpoint(0x11),
        )
        .expect("issue IFA on bitcoin");
        assert_ne!(r.contract_id, r_btc.contract_id);
    }

    #[test]
    fn mint_consumes_allowance_and_yields_bundle() {
        let issuance = issue_ifa(
            ChainNet::LiquidTestnet,
            "LiquidRgbUSD",
            "LRUSD",
            1_000_000,
            fake_outpoint(0x21),
        )
        .expect("issue");
        let genesis_opid = OpId::from(issuance.contract_id.to_byte_array());

        let (bundle_id, transition) = build_mint(
            issuance.contract_id,
            genesis_opid,
            0,
            1_000_000,
            250_000,
            fake_outpoint(0x22),       // recipient
            Some(fake_outpoint(0x23)), // next gate
        )
        .expect("mint");
        assert_eq!(transition.contract_id, issuance.contract_id);
        assert!(bundle_id.to_byte_array().iter().any(|b| *b != 0));

        // Exhausting mint needs no new gate seal.
        let (_, t2) = build_mint(
            issuance.contract_id,
            genesis_opid,
            0,
            1_000_000,
            1_000_000,
            fake_outpoint(0x24),
            None,
        )
        .expect("exhausting mint");
        assert_eq!(t2.contract_id, issuance.contract_id);

        // Over-mint is rejected before it ever reaches the schema.
        assert!(build_mint(
            issuance.contract_id,
            genesis_opid,
            0,
            1_000_000,
            1_000_001,
            fake_outpoint(0x25),
            None,
        )
        .is_err());
    }

    #[test]
    fn backing_check_accepts_exact_and_rejects_short() {
        use elements::confidential::{Asset, Nonce, Value};
        use elements::{AssetId, Script, Transaction, TxOut, TxOutWitness};

        let vault_spk = vec![0x00u8, 0x20, 0xAB, 0xCD];
        let asset = AssetId::from_slice(&[7u8; 32]).unwrap();
        let other = AssetId::from_slice(&[9u8; 32]).unwrap();

        let out = |spk: &[u8], a: AssetId, v: u64| TxOut {
            asset: Asset::Explicit(a),
            value: Value::Explicit(v),
            nonce: Nonce::Null,
            script_pubkey: Script::from(spk.to_vec()),
            witness: TxOutWitness::default(),
        };
        let tx = |outputs: Vec<TxOut>| Transaction {
            version: 2,
            lock_time: elements::LockTime::ZERO,
            input: vec![],
            output: outputs,
        };

        // Exact backing passes.
        let good = tx(vec![out(&vault_spk, asset, 500)]);
        assert_eq!(verify_backing(&good, &vault_spk, &asset, 500).unwrap(), 500);

        // Split across two vault outputs passes.
        let split = tx(vec![
            out(&vault_spk, asset, 300),
            out(&vault_spk, asset, 200),
        ]);
        assert_eq!(
            verify_backing(&split, &vault_spk, &asset, 500).unwrap(),
            500
        );

        // Short backing fails.
        let short = tx(vec![out(&vault_spk, asset, 499)]);
        assert!(verify_backing(&short, &vault_spk, &asset, 500).is_err());

        // Wrong asset to the vault does not count.
        let wrong = tx(vec![out(&vault_spk, other, 500)]);
        assert!(verify_backing(&wrong, &vault_spk, &asset, 500).is_err());

        // Locking to a different script does not count.
        let elsewhere = tx(vec![out(&[0x00, 0x20, 0xFF], asset, 500)]);
        assert!(verify_backing(&elsewhere, &[0x00, 0x20, 0xAB, 0xCD], &asset, 500).is_err());
    }
}
