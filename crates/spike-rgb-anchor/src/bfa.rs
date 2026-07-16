//! BFA — Backed Fungible Asset: a backing-aware RGB schema.
//!
//! The IFA backed mint (see [`crate::mint`]) has one gap: the schema
//! knows nothing about backing. The vault lock lives in the witness
//! transaction, but the *contract* never states the terms, so a
//! validator has nothing contract-committed to check the lock against.
//! The mint amount and the backing amount are only connected by
//! convention.
//!
//! BFA closes that gap at the schema level. It is IFA with one change:
//! genesis MUST carry a `backingTerms` global — the vault scriptPubKey,
//! the Elements backing asset, and the units-per-token rate — committed
//! like every other global, and therefore part of the contract id.
//!
//! That single commitment makes the whole history auditable:
//!
//!   * the terms are bound to the contract id, and every mint's anchor
//!     commitment chains back to it, so nobody can present the same
//!     history under different terms — the anchors stop matching;
//!   * every `TS_INFLATION` transition already commits its minted
//!     amount (`issuedSupply`, enforced by IFA's AluVM arithmetic), and
//!     its witness transaction is already fetched for anchor checking,
//!     so the auditor's rule is mechanical: for every mint,
//!     `vault_locked(witness_tx) >= minted * rate` — no oracle.
//!
//! Stock rgb-consensus never shows the witness transaction to AluVM
//! (`VmContext` carries only contract state), so this rule cannot run
//! *inside* the schema's validator today. BFA therefore splits it:
//! the schema commits the terms; [`audit`]-side validation (this
//! module + [`crate::mint::verify_backing`]) enforces them against the
//! chain. Giving validators witness-tx access, which would fold the
//! audit into the schema itself, is the natural follow-up to the
//! multi-chain RFC (rgb-protocol/rgb-consensus#12).

use anyhow::{Context, Result};
use rgbcore::bitcoin::OutPoint;
use rgbcore::{ChainNet, GenesisSeal, GlobalStateType, Transition};
use rgbstd::containers::ConsignmentExt;
use rgbstd::contract::{ContractBuilder, IssuerWrapper};
use rgbstd::rgbcore::stl::rgb_contract_id_stl;
use rgbstd::schema::{GlobalDetails, GlobalStateSchema, Occurrences, Schema};
use rgbstd::stl::{AssetSpec, ContractTerms, Details, RicardianContract, StandardTypes};
use rgbstd::validation::Scripts;
use rgbstd::{Amount, Identity, Precision};
use schemata::{InflatableFungibleAsset, GS_ISSUED_SUPPLY};
use strict_encoding::{FieldName, TypeName};
use strict_types::TypeSystem;

use crate::mint::{self, IfaIssuance};

/// Genesis global carrying the backing terms. Code chosen clear of
/// every type rgb-schemas assigns (2xxx contract, 3000-3006 extras).
pub const GS_BACKING: GlobalStateType = GlobalStateType::with(3100);

const TERMS_PREFIX: &str = "elements-backing:v1";

/// Fixed genesis timestamp. `issue_contract()` stamps the wall clock
/// into genesis, which changes the contract id on every run; an
/// auditable contract must be re-derivable from its public parameters
/// alone, so BFA pins the timestamp (2025-01-01T00:00:00Z).
pub const GENESIS_TIMESTAMP: i64 = 1_735_689_600;

/// The contract-committed backing terms: which vault script must be
/// paid, in which Elements asset, at what rate per minted unit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackingTerms {
    /// The vault scriptPubKey backing deposits must pay.
    pub vault_spk: Vec<u8>,
    /// The Elements asset (display hex) that counts as backing.
    pub backing_asset: elements::AssetId,
    /// Backing units required per minted unit, as `rate_num/rate_den`.
    pub rate_num: u64,
    pub rate_den: u64,
}

impl BackingTerms {
    /// Canonical string form stored in the `backingTerms` genesis
    /// global: `elements-backing:v1;vault=<spk>;asset=<id>;rate=<n>/<d>`.
    pub fn to_canonical(&self) -> String {
        format!(
            "{TERMS_PREFIX};vault={};asset={};rate={}/{}",
            hex::encode(&self.vault_spk),
            self.backing_asset,
            self.rate_num,
            self.rate_den
        )
    }

    pub fn from_canonical(s: &str) -> Result<Self> {
        let mut vault = None;
        let mut asset = None;
        let mut rate = None;
        let mut parts = s.split(';');
        anyhow::ensure!(
            parts.next() == Some(TERMS_PREFIX),
            "backing terms must start with `{TERMS_PREFIX}`"
        );
        for part in parts {
            match part.split_once('=') {
                Some(("vault", v)) => vault = Some(hex::decode(v).context("vault spk hex")?),
                Some(("asset", v)) => {
                    asset = Some(v.parse::<elements::AssetId>().context("backing asset id")?)
                }
                Some(("rate", v)) => {
                    let (n, d) = v.split_once('/').context("rate must be <num>/<den>")?;
                    rate = Some((n.parse::<u64>()?, d.parse::<u64>()?));
                }
                _ => anyhow::bail!("unknown backing terms field: {part}"),
            }
        }
        let (rate_num, rate_den) = rate.context("missing rate")?;
        anyhow::ensure!(rate_den > 0, "rate denominator must be non-zero");
        Ok(Self {
            vault_spk: vault.context("missing vault")?,
            backing_asset: asset.context("missing asset")?,
            rate_num,
            rate_den,
        })
    }

    /// Backing units a mint of `minted` tokens must lock:
    /// `ceil(minted * rate_num / rate_den)`.
    pub fn required_backing(&self, minted: u64) -> Result<u64> {
        let num = (minted as u128) * (self.rate_num as u128);
        let den = self.rate_den as u128;
        u64::try_from(num.div_ceil(den)).context("required backing overflows u64")
    }
}

/// The BFA schema: IFA plus a mandatory `backingTerms` genesis global.
/// Supply arithmetic is enforced by IFA's unmodified AluVM validators;
/// the new global is committed structural state.
pub fn bfa_schema() -> Schema {
    let types = StandardTypes::with(rgb_contract_id_stl());
    let mut schema = InflatableFungibleAsset::schema();
    schema.name = TypeName::try_from("BackedFungibleAsset".to_owned()).expect("valid type name");
    schema
        .global_types
        .insert(
            GS_BACKING,
            GlobalDetails {
                global_state_schema: GlobalStateSchema::once(types.get("RGBContract.Details")),
                name: FieldName::from("backingTerms"),
            },
        )
        .expect("schema global types within confinement");
    schema
        .genesis
        .globals
        .insert(GS_BACKING, Occurrences::Once)
        .expect("genesis globals within confinement");
    schema
}

pub fn bfa_types() -> TypeSystem {
    StandardTypes::with(rgb_contract_id_stl()).type_system(bfa_schema())
}

/// BFA adds no new validation scripts: the backing rule needs the
/// witness transaction, which AluVM cannot see (see module docs).
pub fn bfa_scripts() -> Scripts {
    InflatableFungibleAsset::scripts()
}

/// Issue a BFA contract: zero circulating supply, the full allowance
/// on the gate seal, and the backing terms committed in genesis.
pub fn issue_bfa(
    chain_net: ChainNet,
    name: &str,
    ticker: &str,
    max_supply: u64,
    gate_seal: OutPoint,
    terms: &BackingTerms,
) -> Result<IfaIssuance> {
    use rgbstd::stl::{Name, Ticker};

    let spec = AssetSpec {
        ticker: Ticker::try_from(ticker.to_owned()).map_err(|e| anyhow::anyhow!("ticker: {e}"))?,
        name: Name::try_from(name.to_owned()).map_err(|e| anyhow::anyhow!("name: {e}"))?,
        details: None,
        precision: Precision::Indivisible,
    };
    let contract_terms = ContractTerms {
        text: RicardianContract::default(),
        media: None,
    };
    let backing = Details::try_from(terms.to_canonical())
        .map_err(|e| anyhow::anyhow!("backing terms as Details: {e}"))?;

    let gate: GenesisSeal = GenesisSeal::with_blinding(gate_seal.txid, gate_seal.vout, 0u64);

    let consignment = ContractBuilder::with(
        Identity::default(),
        bfa_schema(),
        bfa_types(),
        bfa_scripts(),
        chain_net,
    )
    .add_global_state(FieldName::from("spec"), spec)
    .context("add spec")?
    .add_global_state(FieldName::from("terms"), contract_terms)
    .context("add terms")?
    .add_global_state(FieldName::from("backingTerms"), backing)
    .context("add backingTerms")?
    .add_global_state(FieldName::from("issuedSupply"), Amount::from(0u64))
    .context("add issuedSupply")?
    .add_global_state(FieldName::from("maxSupply"), Amount::from(max_supply))
    .context("add maxSupply")?
    .add_fungible_state(FieldName::from("inflationAllowance"), gate, max_supply)
    .context("add inflationAllowance")?
    .issue_contract_raw(GENESIS_TIMESTAMP)
    .map_err(|e| anyhow::anyhow!("issue_contract: {e:?}"))?;

    Ok(IfaIssuance {
        contract_id: consignment.contract_id(),
        gate_seal_outpoint: gate_seal,
        max_supply,
    })
}

/// Build a BFA mint: the same `TS_INFLATION` transition as IFA (same
/// AluVM supply arithmetic), under the BFA schema.
#[allow(clippy::too_many_arguments)]
pub fn build_bfa_mint(
    contract_id: rgbcore::ContractId,
    gate_opid: rgbcore::OpId,
    gate_opout_no: u16,
    allowance_before: u64,
    mint_amount: u64,
    recipient_seal: OutPoint,
    new_gate_seal: Option<OutPoint>,
) -> Result<(rgbcore::BundleId, Transition)> {
    mint::build_mint_with(
        bfa_schema(),
        bfa_types(),
        contract_id,
        gate_opid,
        gate_opout_no,
        allowance_before,
        mint_amount,
        recipient_seal,
        new_gate_seal,
    )
}

/// Read the minted amount a `TS_INFLATION` transition *commits to*:
/// its `issuedSupply` global. IFA's AluVM validator enforces that this
/// equals the sum of new asset allocations, so it is the committed
/// mint size, not a claim.
pub fn minted_amount(transition: &Transition) -> Result<u64> {
    let values = transition
        .globals
        .get(&GS_ISSUED_SUPPLY)
        .context("TS_INFLATION transition has no issuedSupply global")?;
    let value = values.first().context("empty issuedSupply global")?;
    let bytes: &[u8] = value.as_ref();
    anyhow::ensure!(
        bytes.len() == 8,
        "issuedSupply must be a strict-encoded u64, got {} bytes",
        bytes.len()
    );
    let mut le = [0u8; 8];
    le.copy_from_slice(bytes);
    Ok(u64::from_le_bytes(le))
}

/// One mint's audit verdict: what the contract committed to, what the
/// terms require, and what the chain actually locked.
#[derive(Debug)]
pub struct MintAudit {
    pub minted: u64,
    pub required: u64,
    pub locked: u64,
}

/// The backing rule, run against one mint's witness transaction:
/// read the committed mint size off the transition, derive the
/// requirement from the genesis terms, and check the vault deposit in
/// the witness transaction. Anchor and seal checks are the caller's
/// (they are chain-level, not schema-level).
pub fn audit_mint(
    terms: &BackingTerms,
    transition: &Transition,
    witness_tx: &elements::Transaction,
) -> Result<MintAudit> {
    let minted = minted_amount(transition)?;
    let required = terms.required_backing(minted)?;
    let locked =
        mint::verify_backing(witness_tx, &terms.vault_spk, &terms.backing_asset, required)?;
    Ok(MintAudit {
        minted,
        required,
        locked,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgbcore::bitcoin::{hashes::Hash, Txid};

    fn outpoint(seed: u8) -> OutPoint {
        let mut bytes = [0u8; 32];
        bytes[0] = seed;
        OutPoint::new(Txid::from_byte_array(bytes), 0)
    }

    fn demo_terms() -> BackingTerms {
        BackingTerms {
            vault_spk: hex::decode("0014aabbccddeeff00112233445566778899aabbccdd").unwrap(),
            backing_asset: "5ac9f65c0efcc4775e0baec4ec03abdde22473cd3cf33c0419ca290e0751b225"
                .parse()
                .unwrap(),
            rate_num: 1,
            rate_den: 1,
        }
    }

    #[test]
    fn terms_roundtrip() {
        let t = demo_terms();
        let s = t.to_canonical();
        assert!(s.len() <= 255, "terms must fit RGBContract.Details");
        assert_eq!(BackingTerms::from_canonical(&s).unwrap(), t);
    }

    #[test]
    fn required_backing_rounds_up() {
        let mut t = demo_terms();
        t.rate_num = 1;
        t.rate_den = 3;
        assert_eq!(t.required_backing(10).unwrap(), 4); // ceil(10/3)
        t.rate_num = 2;
        t.rate_den = 1;
        assert_eq!(t.required_backing(10).unwrap(), 20);
    }

    #[test]
    fn bfa_is_a_distinct_schema_and_terms_move_the_contract_id() {
        use schemata::IFA_SCHEMA_ID;
        let schema = bfa_schema();
        assert_ne!(schema.schema_id(), IFA_SCHEMA_ID);

        let a = issue_bfa(
            ChainNet::LiquidTestnet,
            "LiquidRgbUSD",
            "LRUSD",
            1_000_000,
            outpoint(0x31),
            &demo_terms(),
        )
        .expect("issue");
        let mut other = demo_terms();
        other.rate_num = 2;
        let b = issue_bfa(
            ChainNet::LiquidTestnet,
            "LiquidRgbUSD",
            "LRUSD",
            1_000_000,
            outpoint(0x31),
            &other,
        )
        .expect("issue");
        assert_ne!(
            a.contract_id, b.contract_id,
            "backing terms must be committed in the contract id"
        );
    }

    #[test]
    fn mint_commits_its_size() {
        let issuance = issue_bfa(
            ChainNet::LiquidTestnet,
            "LiquidRgbUSD",
            "LRUSD",
            1_000_000,
            outpoint(0x41),
            &demo_terms(),
        )
        .expect("issue");
        let genesis_opid = rgbcore::OpId::from(issuance.contract_id.to_byte_array());
        let (_bundle, transition) = build_bfa_mint(
            issuance.contract_id,
            genesis_opid,
            0,
            1_000_000,
            250_000,
            outpoint(0x42),
            Some(outpoint(0x43)),
        )
        .expect("mint");
        assert_eq!(minted_amount(&transition).unwrap(), 250_000);
    }

    #[test]
    fn audit_math_binds_mint_to_backing() {
        use elements::confidential::{Asset, Nonce, Value};
        let terms = demo_terms();

        let issuance = issue_bfa(
            ChainNet::LiquidTestnet,
            "LiquidRgbUSD",
            "LRUSD",
            1_000_000,
            outpoint(0x51),
            &terms,
        )
        .expect("issue");
        let genesis_opid = rgbcore::OpId::from(issuance.contract_id.to_byte_array());
        let (_bundle, transition) = build_bfa_mint(
            issuance.contract_id,
            genesis_opid,
            0,
            1_000_000,
            30_000,
            outpoint(0x52),
            Some(outpoint(0x53)),
        )
        .expect("mint");

        let vault_out = |amount: u64| elements::TxOut {
            asset: Asset::Explicit(terms.backing_asset),
            value: Value::Explicit(amount),
            nonce: Nonce::Null,
            script_pubkey: elements::Script::from(terms.vault_spk.clone()),
            witness: elements::TxOutWitness::default(),
        };
        let tx = |vault_amount: u64| elements::Transaction {
            version: 2,
            lock_time: elements::LockTime::ZERO,
            input: vec![],
            output: vec![vault_out(vault_amount)],
        };

        let ok = audit_mint(&terms, &transition, &tx(30_000)).expect("fully backed");
        assert_eq!(
            (ok.minted, ok.required, ok.locked),
            (30_000, 30_000, 30_000)
        );

        let err = audit_mint(&terms, &transition, &tx(29_999)).unwrap_err();
        assert!(err.to_string().contains("under-backed"), "got: {err}");
    }
}
