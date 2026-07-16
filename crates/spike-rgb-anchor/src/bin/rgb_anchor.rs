//! `rgb-anchor` — drive RGB anchor flows on Bitcoin and Liquid regtest
//! using the rgb-protocol 0.11 stack.
//!
//! Run `rgb-anchor --help` for the full subcommand list. In brief:
//! `build` / `rgb20-transfer` produce an anchor and its P2TR address;
//! `verify` / `verify-patched` check an anchor against an on-chain
//! witness transaction; the `swap-*` subcommands build the hashlock
//! addresses and claim transactions for a cross-chain atomic swap.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use rgbcore::bitcoin::OutPoint;
use rgbcore::commit_verify::CommitId;
use rgbcore::ChainNet;
use spike_rgb_anchor::{
    anchor::{AnchorEntry, LiquidAnchor},
    bundle, liquid_dbc, mpc, patched_anchor, rgb20, rgb_real,
    seal::{self, LiquidSeal},
};

#[derive(Parser)]
#[command(
    name = "rgb-anchor",
    about = "Real-RGB anchor on Liquid (rgb-consensus 0.11)"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Build the MPC root + tapret address.
    Build {
        /// One or more contract labels. Each becomes one MPC entry.
        /// Default: two contracts ("usdt-liquid", "xaut-liquid").
        #[arg(long, num_args = 1.., default_values_t = vec!["usdt-liquid".to_string(), "xaut-liquid".to_string()])]
        contracts: Vec<String>,
        /// X-only internal key (32 bytes hex).
        #[arg(
            long,
            default_value = "d6889cb081036e0faefa3a35157ad71086b123b2b144b649798b494c300a961d"
        )]
        internal_key: String,
        /// Static entropy for the MPC tree (deterministic).
        #[arg(long, default_value_t = 0xC0FFEE_u64)]
        entropy: u64,
        /// ChainNet variant to stamp on the contracts.
        #[arg(long, default_value = "liquid-testnet")]
        chain_net: String,
        /// Optional seal outpoint `<txid>:<vout>`. When set, the
        /// verifier will additionally confirm the witness tx spends
        /// this UTXO, i.e. the seal is closed by the same tx that
        /// carries the tapret commitment.
        #[arg(long)]
        seal: Option<String>,
    },
    /// Verify a saved anchor against the on-chain Liquid tx.
    Verify {
        #[arg(long)]
        anchor: std::path::PathBuf,
    },
    /// Verify an anchor through the patched rgbcore::dbc::Anchor::verify,
    /// against the witness tx via the WitnessTx trait.
    VerifyPatched {
        #[arg(long)]
        anchor: std::path::PathBuf,
        /// Which entry's protocol_id to verify the inclusion for
        /// (default: first entry).
        #[arg(long, default_value_t = 0)]
        entry: usize,
        /// Fetch the witness tx from bitcoind instead of elementsd.
        /// Use this for Bitcoin-leg anchors.
        #[arg(long, default_value_t = false)]
        bitcoin: bool,
    },
    /// Real RGB20 (NIA) transfer. Issues a NIA contract, builds a
    /// transfer transition (sender to recipient plus change), wraps it
    /// in a TransitionBundle, computes the (ContractId, BundleId) MPC
    /// entry, and emits the anchor JSON ready to broadcast.
    Rgb20Transfer {
        #[arg(long, default_value = "KaleidoLiquidUSD")]
        name: String,
        #[arg(long, default_value = "kLUSD")]
        ticker: String,
        #[arg(long, default_value_t = 1_000_000)]
        supply: u64,
        #[arg(long, default_value_t = 600_000)]
        send: u64,
        /// `<txid>:<vout>` — the issuance seal (the UTXO that holds the
        /// sender's NIA allocation).
        #[arg(long)]
        alice_seal: String,
        /// `<txid>:<vout>` — the recipient's receive seal.
        #[arg(long)]
        bob_seal: String,
        /// `<txid>:<vout>` — the sender's change seal. If omitted, send
        /// must equal supply.
        #[arg(long)]
        change_seal: Option<String>,
        /// For CHAINED transfers: OpId (hex) of the transition whose
        /// allocation is consumed. Default: the contract genesis.
        #[arg(long)]
        consume_opid: Option<String>,
        /// For chained transfers: the amount carried by the consumed
        /// allocation (opout index 0). Default: `--supply`.
        #[arg(long)]
        prev_amount: Option<u64>,
        /// For chained transfers: the outpoint the consumed allocation
        /// sits on (the seal this witness tx must close). Default:
        /// `--alice-seal`.
        #[arg(long)]
        close_seal: Option<String>,
        /// X-only internal key (32 bytes hex).
        #[arg(
            long,
            default_value = "d6889cb081036e0faefa3a35157ad71086b123b2b144b649798b494c300a961d"
        )]
        internal_key: String,
        #[arg(long, default_value_t = 0xC0FFEE_u64)]
        entropy: u64,
        /// Which chain to issue on: `liquid-testnet` or `bitcoin-regtest`.
        /// Controls both the RGB `ChainNet` stamp and the P2TR address HRP.
        #[arg(long, default_value = "liquid-testnet")]
        chain_net: String,
    },

    /// Derive a hashlock P2WSH address for a swap leg.
    SwapHashlock {
        /// Preimage hex (any length; 32 bytes recommended).
        #[arg(long)]
        preimage: String,
        /// Address HRP: `bcrt` (Bitcoin regtest) or `ert` (Liquid regtest).
        #[arg(long)]
        hrp: String,
    },

    /// Build the Bitcoin tx that spends a hashlock UTXO by revealing
    /// the preimage. Prints raw tx hex.
    SwapClaimBtc {
        #[arg(long)]
        prev_txid: String,
        #[arg(long)]
        prev_vout: u32,
        #[arg(long)]
        input_value_sat: u64,
        #[arg(long, default_value_t = 500)]
        fee_sat: u64,
        /// Destination scriptPubKey hex (the claimer's output).
        #[arg(long)]
        dest_spk: String,
        #[arg(long)]
        preimage: String,
    },

    /// Build the Elements/Liquid tx that spends a hashlock UTXO by
    /// revealing the preimage. Prints raw tx hex.
    SwapClaimLiquid {
        #[arg(long)]
        prev_txid: String,
        #[arg(long)]
        prev_vout: u32,
        #[arg(long)]
        output_value_sat: u64,
        #[arg(long, default_value_t = 500)]
        fee_sat: u64,
        #[arg(long)]
        dest_spk: String,
        #[arg(
            long,
            default_value = "b2e15d0d7a0c94e4e2ce0fe6e8691b9e451377f6e46e8045a86f7c4b5d4f0f23"
        )]
        lbtc_asset: String,
        #[arg(long)]
        preimage: String,
    },

    /// Issue an IFA (inflatable) contract and build a backed mint:
    /// a TS_INFLATION transition consuming the gate seal's allowance.
    /// The witness tx is expected to lock the backing asset; check it
    /// afterwards with `verify-backed-mint`.
    IfaMint {
        #[arg(long, default_value = "LiquidRgbUSD")]
        name: String,
        #[arg(long, default_value = "LRUSD")]
        ticker: String,
        #[arg(long, default_value_t = 1_000_000)]
        max_supply: u64,
        /// Units minted by this transition (consumes that much allowance).
        #[arg(long, default_value_t = 250_000)]
        mint: u64,
        /// `<txid>:<vout>` — UTXO holding the inflation allowance.
        #[arg(long)]
        gate_seal: String,
        /// `<txid>:<vout>` — seal receiving the minted units.
        #[arg(long)]
        recipient_seal: String,
        /// `<txid>:<vout>` — seal receiving the remaining allowance
        /// (required unless the mint exhausts it).
        #[arg(long)]
        new_gate_seal: Option<String>,
        /// For chained mints: the genesis gate seal that defines the
        /// contract, when `--gate-seal` is a later gate being spent.
        #[arg(long)]
        orig_gate_seal: Option<String>,
        /// For chained mints: OpId (hex) of the transition whose
        /// allowance is consumed. Default: the contract genesis.
        #[arg(long)]
        consume_opid: Option<String>,
        /// For chained mints: the allowance carried by the consumed
        /// assignment. Default: `--max-supply` (a first mint).
        #[arg(long)]
        allowance: Option<u64>,
        /// X-only internal key (32 bytes hex).
        #[arg(
            long,
            default_value = "d6889cb081036e0faefa3a35157ad71086b123b2b144b649798b494c300a961d"
        )]
        internal_key: String,
        #[arg(long, default_value_t = 0xC0FFEE_u64)]
        entropy: u64,
        #[arg(long, default_value = "liquid-testnet")]
        chain_net: String,
    },

    /// Mint under the BFA (Backed Fungible Asset) schema: IFA plus
    /// contract-committed backing terms in genesis. Same outputs as
    /// `ifa-mint` (addr, mpc root, transition OpId on stdout).
    BfaMint {
        #[arg(long, default_value = "LiquidRgbUSD")]
        name: String,
        #[arg(long, default_value = "LRUSD")]
        ticker: String,
        #[arg(long, default_value_t = 1_000_000)]
        max_supply: u64,
        /// Canonical backing terms committed in genesis:
        /// `elements-backing:v1;vault=<spk>;asset=<id>;rate=<n>/<d>`.
        #[arg(long)]
        backing: String,
        #[arg(long, default_value_t = 250_000)]
        mint: u64,
        #[arg(long)]
        gate_seal: String,
        #[arg(long)]
        recipient_seal: String,
        #[arg(long)]
        new_gate_seal: Option<String>,
        #[arg(long)]
        orig_gate_seal: Option<String>,
        #[arg(long)]
        consume_opid: Option<String>,
        #[arg(long)]
        allowance: Option<u64>,
        #[arg(
            long,
            default_value = "d6889cb081036e0faefa3a35157ad71086b123b2b144b649798b494c300a961d"
        )]
        internal_key: String,
        #[arg(long, default_value_t = 0xC0FFEE_u64)]
        entropy: u64,
        #[arg(long, default_value = "liquid-testnet")]
        chain_net: String,
    },

    /// Audit a BFA contract's FULL mint history against the chain.
    /// Rebuilds the committed history (genesis with backing terms,
    /// every mint transition) from the history file, then checks for
    /// every mint: the gate seal was closed by its witness tx, the
    /// witness tx carries the anchor commitment, and the vault locked
    /// at least `minted × rate` of the backing asset. Any lie in the
    /// history file breaks the anchor match; any under-backed mint
    /// fails the backing rule. Exits non-zero if any mint fails.
    BfaAudit {
        /// JSON history file; see `bfa_history` in scripts/.
        #[arg(long)]
        history: std::path::PathBuf,
    },

    /// Verify a backed mint: the standard anchor + seal checks, plus
    /// the backing rule: the witness tx must lock >= `required` units
    /// of `backing_asset` into the `vault_spk` output.
    VerifyBackedMint {
        #[arg(long)]
        anchor: std::path::PathBuf,
        /// Vault scriptPubKey hex the backing must be locked to.
        #[arg(long)]
        vault_spk: String,
        /// Backing Elements asset id (display hex).
        #[arg(long)]
        backing_asset: String,
        /// Minimum backing amount, in explicit units (asset satoshis).
        #[arg(long)]
        required: u64,
    },

    /// Derive a full-HTLC P2WSH address (claim branch = preimage +
    /// claimer key; refund branch = CSV delay + refund key). Demo keys
    /// are derived from labels: sk = SHA256(label).
    HtlcAddress {
        /// SHA256 hash locking the claim branch (32-byte hex).
        #[arg(long)]
        hash: String,
        /// Label for the claimer demo key.
        #[arg(long)]
        claimer: String,
        /// Label for the refund demo key.
        #[arg(long)]
        refund: String,
        /// CSV delay (relative blocks) on the refund branch.
        #[arg(long, default_value_t = 5)]
        csv_delay: u32,
        /// Address HRP: `bcrt` (Bitcoin regtest) or `ert` (Liquid regtest).
        #[arg(long)]
        hrp: String,
    },

    /// Build + sign the Bitcoin tx spending an HTLC UTXO (claim or
    /// refund branch). Prints raw tx hex.
    HtlcSpendBtc {
        #[arg(long)]
        prev_txid: String,
        #[arg(long)]
        prev_vout: u32,
        #[arg(long)]
        input_value_sat: u64,
        #[arg(long, default_value_t = 500)]
        fee_sat: u64,
        /// Destination scriptPubKey hex.
        #[arg(long)]
        dest_spk: String,
        /// The hash the HTLC script was built with (32-byte hex).
        #[arg(long)]
        hash: String,
        #[arg(long)]
        claimer: String,
        #[arg(long)]
        refund: String,
        #[arg(long, default_value_t = 5)]
        csv_delay: u32,
        /// `claim` or `refund`.
        #[arg(long)]
        branch: String,
        /// Preimage hex (claim branch only; may deliberately mismatch
        /// `--hash` for negative tests).
        #[arg(long)]
        preimage: Option<String>,
        /// Optional anchor output (e.g. a tapret SPK) placed at vout 0.
        #[arg(long)]
        anchor_spk: Option<String>,
        #[arg(long, default_value_t = 500)]
        anchor_value_sat: u64,
    },

    /// Build + sign the Elements/Liquid tx spending an HTLC UTXO
    /// (claim or refund branch). Prints raw tx hex.
    HtlcSpendLiquid {
        #[arg(long)]
        prev_txid: String,
        #[arg(long)]
        prev_vout: u32,
        #[arg(long)]
        input_value_sat: u64,
        #[arg(long, default_value_t = 500)]
        fee_sat: u64,
        #[arg(long)]
        dest_spk: String,
        #[arg(
            long,
            default_value = "b2e15d0d7a0c94e4e2ce0fe6e8691b9e451377f6e46e8045a86f7c4b5d4f0f23"
        )]
        lbtc_asset: String,
        #[arg(long)]
        hash: String,
        #[arg(long)]
        claimer: String,
        #[arg(long)]
        refund: String,
        #[arg(long, default_value_t = 5)]
        csv_delay: u32,
        /// `claim` or `refund`.
        #[arg(long)]
        branch: String,
        #[arg(long)]
        preimage: Option<String>,
        /// Optional anchor output (e.g. a tapret SPK) placed at vout 0.
        #[arg(long)]
        anchor_spk: Option<String>,
        #[arg(long, default_value_t = 500)]
        anchor_value_sat: u64,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    match Cli::parse().cmd {
        Cmd::Build {
            contracts,
            internal_key,
            entropy,
            chain_net,
            seal,
        } => {
            let parsed_seal = match seal {
                Some(s) => Some(parse_seal(&s)?),
                None => None,
            };
            build(&contracts, &internal_key, entropy, &chain_net, parsed_seal)
        }
        Cmd::Verify { anchor } => verify(&anchor).await,
        Cmd::VerifyPatched {
            anchor,
            entry,
            bitcoin,
        } => verify_patched(&anchor, entry, bitcoin).await,
        Cmd::Rgb20Transfer {
            name,
            ticker,
            supply,
            send,
            alice_seal,
            bob_seal,
            change_seal,
            consume_opid,
            prev_amount,
            close_seal,
            internal_key,
            entropy,
            chain_net,
        } => rgb20_transfer(
            &name,
            &ticker,
            supply,
            send,
            &alice_seal,
            &bob_seal,
            change_seal.as_deref(),
            consume_opid.as_deref(),
            prev_amount,
            close_seal.as_deref(),
            &internal_key,
            entropy,
            &chain_net,
        ),
        Cmd::SwapHashlock { preimage, hrp } => swap_hashlock(&preimage, &hrp),
        Cmd::SwapClaimBtc {
            prev_txid,
            prev_vout,
            input_value_sat,
            fee_sat,
            dest_spk,
            preimage,
        } => swap_claim_btc(
            &prev_txid,
            prev_vout,
            input_value_sat,
            fee_sat,
            &dest_spk,
            &preimage,
        ),
        Cmd::SwapClaimLiquid {
            prev_txid,
            prev_vout,
            output_value_sat,
            fee_sat,
            dest_spk,
            lbtc_asset,
            preimage,
        } => swap_claim_liquid(
            &prev_txid,
            prev_vout,
            output_value_sat,
            fee_sat,
            &dest_spk,
            &lbtc_asset,
            &preimage,
        ),
        Cmd::IfaMint {
            name,
            ticker,
            max_supply,
            mint,
            gate_seal,
            recipient_seal,
            new_gate_seal,
            orig_gate_seal,
            consume_opid,
            allowance,
            internal_key,
            entropy,
            chain_net,
        } => mint_flow(
            &name,
            &ticker,
            max_supply,
            None,
            mint,
            &gate_seal,
            &recipient_seal,
            new_gate_seal.as_deref(),
            orig_gate_seal.as_deref(),
            consume_opid.as_deref(),
            allowance,
            &internal_key,
            entropy,
            &chain_net,
        ),
        Cmd::BfaMint {
            name,
            ticker,
            max_supply,
            backing,
            mint,
            gate_seal,
            recipient_seal,
            new_gate_seal,
            orig_gate_seal,
            consume_opid,
            allowance,
            internal_key,
            entropy,
            chain_net,
        } => mint_flow(
            &name,
            &ticker,
            max_supply,
            Some(&backing),
            mint,
            &gate_seal,
            &recipient_seal,
            new_gate_seal.as_deref(),
            orig_gate_seal.as_deref(),
            consume_opid.as_deref(),
            allowance,
            &internal_key,
            entropy,
            &chain_net,
        ),
        Cmd::BfaAudit { history } => bfa_audit(&history).await,
        Cmd::VerifyBackedMint {
            anchor,
            vault_spk,
            backing_asset,
            required,
        } => verify_backed_mint(&anchor, &vault_spk, &backing_asset, required).await,
        Cmd::HtlcAddress {
            hash,
            claimer,
            refund,
            csv_delay,
            hrp,
        } => htlc_address(&hash, &claimer, &refund, csv_delay, &hrp),
        Cmd::HtlcSpendBtc {
            prev_txid,
            prev_vout,
            input_value_sat,
            fee_sat,
            dest_spk,
            hash,
            claimer,
            refund,
            csv_delay,
            branch,
            preimage,
            anchor_spk,
            anchor_value_sat,
        } => htlc_spend(
            Chain::Bitcoin,
            &prev_txid,
            prev_vout,
            input_value_sat,
            fee_sat,
            &dest_spk,
            None,
            &hash,
            &claimer,
            &refund,
            csv_delay,
            &branch,
            preimage.as_deref(),
            anchor_spk.as_deref(),
            anchor_value_sat,
        ),
        Cmd::HtlcSpendLiquid {
            prev_txid,
            prev_vout,
            input_value_sat,
            fee_sat,
            dest_spk,
            lbtc_asset,
            hash,
            claimer,
            refund,
            csv_delay,
            branch,
            preimage,
            anchor_spk,
            anchor_value_sat,
        } => htlc_spend(
            Chain::Liquid,
            &prev_txid,
            prev_vout,
            input_value_sat,
            fee_sat,
            &dest_spk,
            Some(&lbtc_asset),
            &hash,
            &claimer,
            &refund,
            csv_delay,
            &branch,
            preimage.as_deref(),
            anchor_spk.as_deref(),
            anchor_value_sat,
        ),
    }
}

enum Chain {
    Bitcoin,
    Liquid,
}

fn htlc_address(
    hash_hex: &str,
    claimer_label: &str,
    refund_label: &str,
    csv_delay: u32,
    hrp: &str,
) -> Result<()> {
    use spike_rgb_anchor::swap::htlc;
    let hash: [u8; 32] = hex::decode(hash_hex)
        .context("hash hex")?
        .try_into()
        .map_err(|_| anyhow::anyhow!("hash must be 32 bytes"))?;
    let (_, claimer_pk) = htlc::demo_keypair(claimer_label)?;
    let (_, refund_pk) = htlc::demo_keypair(refund_label)?;
    let ws = htlc::htlc_witness_script(&hash, &claimer_pk, &refund_pk, csv_delay);
    println!(
        "{}",
        serde_json::json!({
            "hash_hex": hash_hex,
            "address": htlc::p2wsh_address(hrp, &ws)?,
            "spk_hex": hex::encode(htlc::p2wsh_spk(&ws)),
            "witness_script_hex": hex::encode(&ws),
            "claimer_pk": hex::encode(claimer_pk),
            "refund_pk": hex::encode(refund_pk),
            "csv_delay": csv_delay,
        })
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn htlc_spend(
    chain: Chain,
    prev_txid: &str,
    prev_vout: u32,
    input_value_sat: u64,
    fee_sat: u64,
    dest_spk_hex: &str,
    lbtc_asset: Option<&str>,
    hash_hex: &str,
    claimer_label: &str,
    refund_label: &str,
    csv_delay: u32,
    branch: &str,
    preimage_hex: Option<&str>,
    anchor_spk_hex: Option<&str>,
    anchor_value_sat: u64,
) -> Result<()> {
    use spike_rgb_anchor::swap::htlc::{self, AnchorOut, HtlcSpend};

    let hash: [u8; 32] = hex::decode(hash_hex)
        .context("hash hex")?
        .try_into()
        .map_err(|_| anyhow::anyhow!("hash must be 32 bytes"))?;
    let dest_spk = hex::decode(dest_spk_hex).context("dest_spk hex")?;
    let (claimer_sk, claimer_pk) = htlc::demo_keypair(claimer_label)?;
    let (refund_sk, refund_pk) = htlc::demo_keypair(refund_label)?;
    let ws = htlc::htlc_witness_script(&hash, &claimer_pk, &refund_pk, csv_delay);

    let preimage_bytes;
    let (spend, signer_sk) = match branch {
        "claim" => {
            preimage_bytes =
                hex::decode(preimage_hex.context("--preimage is required for the claim branch")?)
                    .context("preimage hex")?;
            (
                HtlcSpend::Claim {
                    preimage: &preimage_bytes,
                },
                claimer_sk,
            )
        }
        "refund" => (HtlcSpend::Refund, refund_sk),
        other => anyhow::bail!("--branch must be `claim` or `refund`, got `{other}`"),
    };

    let anchor = anchor_spk_hex
        .map(|s| -> Result<AnchorOut> {
            Ok(AnchorOut {
                spk: hex::decode(s).context("anchor_spk hex")?,
                value_sat: anchor_value_sat,
            })
        })
        .transpose()?;
    let anchor_total = anchor.as_ref().map(|a| a.value_sat).unwrap_or(0);
    let output_value = input_value_sat
        .checked_sub(fee_sat + anchor_total)
        .context("fee + anchor exceed input value")?;

    let raw = match chain {
        Chain::Bitcoin => htlc::build_htlc_spend_btc(
            prev_txid,
            prev_vout,
            input_value_sat,
            output_value,
            &dest_spk,
            &ws,
            spend,
            csv_delay,
            &signer_sk,
            anchor,
        )?,
        Chain::Liquid => htlc::build_htlc_spend_liquid(
            prev_txid,
            prev_vout,
            input_value_sat,
            output_value,
            fee_sat,
            &dest_spk,
            lbtc_asset.context("lbtc_asset required for Liquid")?,
            &ws,
            spend,
            csv_delay,
            &signer_sk,
            anchor,
        )?,
    };
    println!("{raw}");
    Ok(())
}

fn swap_hashlock(preimage_hex: &str, hrp: &str) -> Result<()> {
    use spike_rgb_anchor::swap;
    let preimage = hex::decode(preimage_hex).context("preimage hex")?;
    let hash = swap::hash_of(&preimage);
    let address = swap::hashlock_address(hrp, &hash)?;
    let spk = swap::hashlock_spk(&hash);
    println!(
        "{}",
        serde_json::json!({
            "hash_hex": hex::encode(hash),
            "address": address,
            "spk_hex": hex::encode(spk),
            "witness_script_hex": hex::encode(swap::hashlock_witness_script(&hash)),
        })
    );
    Ok(())
}

fn swap_claim_btc(
    prev_txid: &str,
    prev_vout: u32,
    input_value_sat: u64,
    fee_sat: u64,
    dest_spk_hex: &str,
    preimage_hex: &str,
) -> Result<()> {
    use spike_rgb_anchor::swap;
    let preimage = hex::decode(preimage_hex).context("preimage hex")?;
    let dest_spk = hex::decode(dest_spk_hex).context("dest_spk hex")?;
    let hash = swap::hash_of(&preimage);
    let output_value = input_value_sat
        .checked_sub(fee_sat)
        .context("fee exceeds input value")?;
    let raw = swap::bitcoin_leg::build_hashlock_spend(
        prev_txid,
        prev_vout,
        input_value_sat,
        output_value,
        &dest_spk,
        &preimage,
        &hash,
    )?;
    println!("{raw}");
    Ok(())
}

fn swap_claim_liquid(
    prev_txid: &str,
    prev_vout: u32,
    output_value_sat: u64,
    fee_sat: u64,
    dest_spk_hex: &str,
    lbtc_asset: &str,
    preimage_hex: &str,
) -> Result<()> {
    use spike_rgb_anchor::swap;
    let preimage = hex::decode(preimage_hex).context("preimage hex")?;
    let dest_spk = hex::decode(dest_spk_hex).context("dest_spk hex")?;
    let hash = swap::hash_of(&preimage);
    let raw = swap::elements_leg::build_hashlock_spend(
        prev_txid,
        prev_vout,
        output_value_sat,
        fee_sat,
        &dest_spk,
        lbtc_asset,
        &preimage,
        &hash,
    )?;
    println!("{raw}");
    Ok(())
}

fn parse_outpoint(s: &str) -> Result<OutPoint> {
    let parsed: OutPoint = s
        .parse()
        .map_err(|e| anyhow::anyhow!("not a valid txid:vout '{s}': {e}"))?;
    Ok(parsed)
}

#[allow(clippy::too_many_arguments)]
fn rgb20_transfer(
    name: &str,
    ticker: &str,
    supply: u64,
    send: u64,
    alice_seal_s: &str,
    bob_seal_s: &str,
    change_seal_s: Option<&str>,
    consume_opid_s: Option<&str>,
    prev_amount: Option<u64>,
    close_seal_s: Option<&str>,
    internal_key_hex: &str,
    entropy: u64,
    chain_net_s: &str,
) -> Result<()> {
    let p = parse32(internal_key_hex, "internal_key")?;
    let alice_seal = parse_outpoint(alice_seal_s)?;
    let bob_seal = parse_outpoint(bob_seal_s)?;
    let change_seal = change_seal_s.map(parse_outpoint).transpose()?;
    let close_seal = close_seal_s
        .map(parse_outpoint)
        .transpose()?
        .unwrap_or(alice_seal);
    let chain_net = parse_chain_net(chain_net_s)?;
    // P2TR address HRP depends on the chain.
    let hrp = match chain_net {
        ChainNet::BitcoinRegtest => "bcrt",
        ChainNet::BitcoinTestnet4 | ChainNet::BitcoinTestnet3 => "tb",
        ChainNet::BitcoinMainnet => "bc",
        ChainNet::LiquidTestnet => "ert", // elementsregtest HRP
        ChainNet::LiquidMainnet => "ex",
        other => anyhow::bail!("unsupported chain_net for rgb20-transfer: {other}"),
    };

    eprintln!("──────────────────────────────────────────────────────────");
    eprintln!(" RGB20 (NIA) transfer — chain_net = {chain_net}");
    eprintln!("──────────────────────────────────────────────────────────");

    // Step 1: real NIA issuance.
    let issuance = rgb20::issue(chain_net, name, ticker, supply, alice_seal)?;
    eprintln!(" contract    : {}", issuance.contract_id);
    eprintln!(" alice seal  : {}:{}", alice_seal.txid, alice_seal.vout);
    eprintln!(" supply      : {} {}", supply, ticker);

    // Step 2: real transfer transition (genesis-consuming or chained).
    let (bundle_id, transition) = match consume_opid_s {
        Some(op) => rgb20::build_transfer_from(
            issuance.contract_id,
            rgbcore::OpId::from(parse32(op, "consume_opid")?),
            0,
            prev_amount.unwrap_or(supply),
            send,
            bob_seal,
            change_seal,
            0,
        )?,
        None => {
            rgb20::build_transfer(issuance.contract_id, supply, send, bob_seal, change_seal, 0)?
        }
    };
    eprintln!(" bundle_id   : {}", hex::encode(bundle_id.to_byte_array()));
    eprintln!(
        " transition  : {} (consumes 1 input, creates {} assignments)",
        hex::encode(transition.commit_id().to_byte_array()),
        transition.assignments.len(),
    );
    eprintln!(
        " sending     : {} {} to seal {}:{}",
        send, ticker, bob_seal.txid, bob_seal.vout
    );
    if let Some(c) = change_seal {
        eprintln!(
            " change      : {} {} to seal {}:{}",
            supply - send,
            ticker,
            c.txid,
            c.vout
        );
    }

    // Step 3: drive into the MPC + tapret pipeline (real RGB inputs).
    let pid = issuance.contract_id.to_byte_array();
    let msg = bundle_id.to_byte_array();
    let entries = vec![mpc::Entry {
        protocol_id: pid,
        message: msg,
    }];
    let (root, _tree) = mpc::build(&entries, entropy)?;
    let committed = liquid_dbc::commit(p, root)?;
    let spk_bytes = hex::decode(&committed.committed_spk_hex)?;
    if !spk_bytes.starts_with(&[0x51, 0x20]) {
        anyhow::bail!(
            "expected P2TR scriptPubKey, got {}",
            committed.committed_spk_hex
        );
    }
    let mut q = [0u8; 32];
    q.copy_from_slice(&spk_bytes[2..34]);
    let addr = spike_tapret::address::encode_p2tr(hrp, &q)?;

    eprintln!(" MPC root    : {}", hex::encode(root));
    eprintln!(" SPK         : {}", committed.committed_spk_hex);
    eprintln!(" P2TR addr   : {addr}");
    eprintln!("──────────────────────────────────────────────────────────");

    let anchor_seal = LiquidSeal {
        txid: format!("{}", close_seal.txid),
        vout: close_seal.vout,
    };
    let anchor = LiquidAnchor {
        txid: String::new(),
        internal_key_hex: hex::encode(p),
        mpc_root_hex: hex::encode(root),
        entries: vec![AnchorEntry {
            protocol_id_hex: hex::encode(pid),
            message_hex: hex::encode(msg),
            label: format!("rgb20:{ticker}"),
        }],
        layer1: format!("{}", chain_net.layer1()),
        chain_net: format!("{chain_net}"),
        static_entropy: entropy,
        seal: Some(anchor_seal),
    };

    println!("{addr}");
    println!("{}", serde_json::to_string(&anchor)?);
    // Third stdout line: this transition's OpId, so a chained transfer
    // can consume the allocation it created (`--consume-opid`).
    println!("{}", hex::encode(transition.commit_id().to_byte_array()));
    Ok(())
}

async fn verify_patched(
    anchor_path: &std::path::Path,
    entry_idx: usize,
    bitcoin: bool,
) -> Result<()> {
    let s = std::fs::read_to_string(anchor_path).context("reading anchor")?;
    let anchor: LiquidAnchor = serde_json::from_str(&s).context("parsing anchor")?;
    if anchor.txid.is_empty() {
        anyhow::bail!("anchor.txid empty — broadcast first");
    }
    let entry_json = anchor
        .entries
        .get(entry_idx)
        .ok_or_else(|| anyhow::anyhow!("no entry at index {entry_idx}"))?;
    let p = parse32(&anchor.internal_key_hex, "internal_key_hex")?;
    let pid = parse32(&entry_json.protocol_id_hex, "protocol_id_hex")?;
    let msg = parse32(&entry_json.message_hex, "message_hex")?;

    // This single-entry build assumes the anchor was built with one
    // entry. A multi-entry anchor would carry a per-protocol
    // `MerkleProof`; here the proof is re-derived from the entries list.
    if anchor.entries.len() != 1 {
        eprintln!(
            "  note: anchor has {} entries; we rebuild with the chosen entry as the single slot",
            anchor.entries.len()
        );
    }
    let rgb_anchor = patched_anchor::build_anchor(pid, msg, anchor.static_entropy, p)?;

    // Fetch witness tx and wrap as our `WitnessTx` impl. The same
    // ConvolveCommitProof path verifies a Bitcoin or a Liquid tx — the
    // only difference here is which node we ask for the raw tx.
    let rpc = if bitcoin {
        spike_env::elements_rpc::ElementsRpc::bitcoind_defaults()
    } else {
        spike_env::elements_rpc::ElementsRpc::from_defaults()
    };
    let wtx = seal::fetch_witness_tx(&rpc, &anchor.txid).await?;

    let root = patched_anchor::verify_anchor_on_liquid(&rgb_anchor, pid, msg, &wtx)?;
    let chain = if bitcoin { "Bitcoin" } else { "Liquid" };
    println!(
        "✓ PATCHED rgbcore::dbc::Anchor::verify accepted {chain} witness tx:\n  \
         tx {} commits to MPC commitment {} (protocol_id={}, message={})",
        anchor.txid,
        hex::encode(root),
        &entry_json.protocol_id_hex[..16],
        &entry_json.message_hex[..16],
    );
    Ok(())
}

fn parse_seal(s: &str) -> Result<LiquidSeal> {
    let (txid, vout_s) = s
        .rsplit_once(':')
        .ok_or_else(|| anyhow::anyhow!("seal must be <txid>:<vout> (got '{s}')"))?;
    let vout: u32 = vout_s.parse().context("seal vout is not a u32")?;
    if txid.len() != 64 || hex::decode(txid).is_err() {
        anyhow::bail!("seal txid is not 64-char hex");
    }
    Ok(LiquidSeal {
        txid: txid.to_lowercase(),
        vout,
    })
}

fn parse_chain_net(s: &str) -> Result<ChainNet> {
    match s {
        "liquid-mainnet" => Ok(ChainNet::LiquidMainnet),
        "liquid-testnet" => Ok(ChainNet::LiquidTestnet),
        "bitcoin-regtest" => Ok(ChainNet::BitcoinRegtest),
        "bitcoin-testnet" => Ok(ChainNet::BitcoinTestnet4),
        "bitcoin-mainnet" => Ok(ChainNet::BitcoinMainnet),
        other => anyhow::bail!("unknown chain_net '{other}'"),
    }
}

fn parse32(s: &str, label: &str) -> Result<[u8; 32]> {
    let v = hex::decode(s).with_context(|| format!("{label} is not hex"))?;
    if v.len() != 32 {
        anyhow::bail!("{label} must be 32 bytes ({} given)", v.len());
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&v);
    Ok(out)
}

fn build(
    contract_labels: &[String],
    internal_key_hex: &str,
    entropy: u64,
    chain_net_s: &str,
    seal: Option<LiquidSeal>,
) -> Result<()> {
    let p = parse32(internal_key_hex, "internal_key")?;
    let chain_net = parse_chain_net(chain_net_s)?;
    let layer1 = chain_net.layer1();

    eprintln!("──────────────────────────────────────────────────────────");
    eprintln!(" chain_net : {chain_net}");
    eprintln!(" layer1    : {layer1}");
    eprintln!(" entries   : {}", contract_labels.len());
    eprintln!("──────────────────────────────────────────────────────────");

    // Build one entry per contract.
    let mut entries_mpc = Vec::new();
    let mut entries_json = Vec::new();
    for label in contract_labels {
        let c = rgb_real::build_contract(chain_net, label)?;
        let bundle_id = bundle::synthetic_bundle_id(label)?;

        let pid = c.contract_id.to_byte_array();
        let msg = bundle_id.to_byte_array();
        eprintln!(" {:15} contract_id={}", label, c.contract_id);
        eprintln!(" {:15}  bundle_id ={}", "", bundle_id);

        entries_mpc.push(mpc::Entry {
            protocol_id: pid,
            message: msg,
        });
        entries_json.push(AnchorEntry {
            protocol_id_hex: hex::encode(pid),
            message_hex: hex::encode(msg),
            label: label.clone(),
        });
    }

    // Build the multi-entry MPC tree.
    let (root, _tree) = mpc::build(&entries_mpc, entropy)?;
    eprintln!(" MPC root  : {}", hex::encode(root));

    // Commit via bp-dbc TapretFirst on a P2TR ScriptBuf.
    let committed = liquid_dbc::commit(p, root)?;
    eprintln!(" SPK       : {}", committed.committed_spk_hex);

    // Encode address from the committed SPK.
    let spk_bytes = hex::decode(&committed.committed_spk_hex)?;
    if !spk_bytes.starts_with(&[0x51, 0x20]) || spk_bytes.len() != 34 {
        anyhow::bail!(
            "unexpected scriptPubKey shape: {}",
            committed.committed_spk_hex
        );
    }
    let mut q = [0u8; 32];
    q.copy_from_slice(&spk_bytes[2..]);
    let addr = spike_tapret::address::encode_p2tr(spike_tapret::address::HRP_REGTEST, &q)?;
    eprintln!(" P2TR addr : {addr}");
    eprintln!("──────────────────────────────────────────────────────────");

    if let Some(ref s) = seal {
        eprintln!(" seal      : {}:{}", s.txid, s.vout);
    } else {
        eprintln!(" seal      : (none — commitment-only anchor)");
    }

    let anchor = LiquidAnchor {
        txid: String::new(),
        internal_key_hex: hex::encode(p),
        mpc_root_hex: hex::encode(root),
        entries: entries_json,
        layer1: format!("{layer1}"),
        chain_net: format!("{chain_net}"),
        static_entropy: entropy,
        seal,
    };

    println!("{addr}");
    println!("{}", serde_json::to_string(&anchor)?);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn mint_flow(
    name: &str,
    ticker: &str,
    max_supply: u64,
    backing: Option<&str>,
    mint_amount: u64,
    gate_seal_s: &str,
    recipient_seal_s: &str,
    new_gate_seal_s: Option<&str>,
    orig_gate_seal_s: Option<&str>,
    consume_opid_s: Option<&str>,
    allowance: Option<u64>,
    internal_key_hex: &str,
    entropy: u64,
    chain_net_s: &str,
) -> Result<()> {
    use spike_rgb_anchor::{bfa, mint};

    let terms = backing
        .map(bfa::BackingTerms::from_canonical)
        .transpose()
        .context("parsing --backing")?;
    let p = parse32(internal_key_hex, "internal_key")?;
    let gate_seal = parse_outpoint(gate_seal_s)?;
    let recipient_seal = parse_outpoint(recipient_seal_s)?;
    let new_gate_seal = new_gate_seal_s.map(parse_outpoint).transpose()?;
    let chain_net = parse_chain_net(chain_net_s)?;
    let hrp = match chain_net {
        ChainNet::BitcoinRegtest => "bcrt",
        ChainNet::LiquidTestnet => "ert",
        ChainNet::LiquidMainnet => "ex",
        other => anyhow::bail!("unsupported chain_net for ifa-mint: {other}"),
    };

    let kind = if terms.is_some() { "BFA" } else { "IFA" };
    eprintln!("──────────────────────────────────────────────────────────");
    eprintln!(" {kind} backed mint — chain_net = {chain_net}");
    eprintln!("──────────────────────────────────────────────────────────");

    // Step 1: (re-)derive the IFA. The contract is defined by the
    // genesis gate seal; for a first mint that is `--gate-seal`
    // itself, for chained mints it is `--orig-gate-seal`.
    let issuance_seal = orig_gate_seal_s
        .map(parse_outpoint)
        .transpose()?
        .unwrap_or(gate_seal);
    let issuance = match &terms {
        Some(t) => bfa::issue_bfa(chain_net, name, ticker, max_supply, issuance_seal, t)?,
        None => mint::issue_ifa(chain_net, name, ticker, max_supply, issuance_seal)?,
    };
    eprintln!(" contract    : {}", issuance.contract_id);
    if let Some(t) = &terms {
        eprintln!(" backing     : {}", t.to_canonical());
    }
    eprintln!(" gate seal   : {}:{}", gate_seal.txid, gate_seal.vout);
    eprintln!(" max supply  : {max_supply} {ticker} (0 issued at genesis)");

    // Step 2: the mint transition consuming the gate allowance. A
    // first mint consumes the genesis assignment; a chained mint
    // consumes the allowance re-assigned by the previous transition.
    let consume_opid = match consume_opid_s {
        Some(s) => rgbcore::OpId::from(parse32(s, "consume_opid")?),
        None => rgbcore::OpId::from(issuance.contract_id.to_byte_array()),
    };
    let allowance_before = allowance.unwrap_or(max_supply);
    let (bundle_id, transition) = match &terms {
        Some(_) => bfa::build_bfa_mint(
            issuance.contract_id,
            consume_opid,
            0,
            allowance_before,
            mint_amount,
            recipient_seal,
            new_gate_seal,
        )?,
        None => mint::build_mint(
            issuance.contract_id,
            consume_opid,
            0,
            allowance_before,
            mint_amount,
            recipient_seal,
            new_gate_seal,
        )?,
    };
    eprintln!(" bundle_id   : {}", hex::encode(bundle_id.to_byte_array()));
    eprintln!(
        " minting     : {} {} to seal {}:{}",
        mint_amount, ticker, recipient_seal.txid, recipient_seal.vout
    );
    if let Some(g) = new_gate_seal {
        eprintln!(
            " allowance   : {} {} rolls to gate seal {}:{}",
            allowance_before - mint_amount,
            ticker,
            g.txid,
            g.vout
        );
    }
    eprintln!(
        " transition  : {} (TS_INFLATION, validated by the schema's AluVM)",
        hex::encode(transition.commit_id().to_byte_array()),
    );

    // Step 3: MPC + tapret, same pipeline as every other anchor here.
    let pid = issuance.contract_id.to_byte_array();
    let msg = bundle_id.to_byte_array();
    let entries = vec![mpc::Entry {
        protocol_id: pid,
        message: msg,
    }];
    let (root, _tree) = mpc::build(&entries, entropy)?;
    let committed = liquid_dbc::commit(p, root)?;
    let spk_bytes = hex::decode(&committed.committed_spk_hex)?;
    if !spk_bytes.starts_with(&[0x51, 0x20]) {
        anyhow::bail!(
            "expected P2TR scriptPubKey, got {}",
            committed.committed_spk_hex
        );
    }
    let mut q = [0u8; 32];
    q.copy_from_slice(&spk_bytes[2..34]);
    let addr = spike_tapret::address::encode_p2tr(hrp, &q)?;

    eprintln!(" MPC root    : {}", hex::encode(root));
    eprintln!(" SPK         : {}", committed.committed_spk_hex);
    eprintln!(" P2TR addr   : {addr}");
    eprintln!("──────────────────────────────────────────────────────────");

    let anchor = LiquidAnchor {
        txid: String::new(),
        internal_key_hex: hex::encode(p),
        mpc_root_hex: hex::encode(root),
        entries: vec![AnchorEntry {
            protocol_id_hex: hex::encode(pid),
            message_hex: hex::encode(msg),
            label: format!("{}-mint:{ticker}", kind.to_lowercase()),
        }],
        layer1: format!("{}", chain_net.layer1()),
        chain_net: format!("{chain_net}"),
        static_entropy: entropy,
        seal: Some(LiquidSeal {
            txid: format!("{}", gate_seal.txid),
            vout: gate_seal.vout,
        }),
    };

    println!("{addr}");
    println!("{}", serde_json::to_string(&anchor)?);
    // Third stdout line: this transition's OpId, so a chained mint can
    // consume the allowance it re-assigned (`--consume-opid`).
    println!("{}", hex::encode(transition.commit_id().to_byte_array()));
    Ok(())
}

/// One mint in a BFA audit history file.
#[derive(serde::Deserialize)]
struct BfaHistoryMint {
    mint: u64,
    recipient_seal: String,
    new_gate_seal: Option<String>,
    witness_txid: String,
}

/// A BFA contract's claimed history: the issuance parameters (which
/// pin the contract id, backing terms included) and every mint. The
/// auditor rebuilds the committed operations from these parameters;
/// any divergence from what was actually anchored on chain shows up
/// as an anchor mismatch.
#[derive(serde::Deserialize)]
struct BfaHistory {
    name: String,
    ticker: String,
    max_supply: u64,
    backing: String,
    genesis_gate_seal: String,
    internal_key: String,
    entropy: u64,
    #[serde(default = "default_chain_net")]
    chain_net: String,
    mints: Vec<BfaHistoryMint>,
}

fn default_chain_net() -> String {
    "liquid-testnet".to_owned()
}

/// Audit a BFA contract's full mint history against the chain: for
/// every mint, the gate seal must be closed by the mint's witness tx,
/// the witness tx must carry the anchor commitment to the rebuilt
/// (committed) transition, and the vault must have locked at least
/// `minted × rate` of the backing asset per the genesis terms.
async fn bfa_audit(history_path: &std::path::Path) -> Result<()> {
    use spike_rgb_anchor::bfa;

    let s = std::fs::read_to_string(history_path).context("reading history file")?;
    let history: BfaHistory = serde_json::from_str(&s).context("parsing history file")?;
    let terms = bfa::BackingTerms::from_canonical(&history.backing)?;
    let chain_net = parse_chain_net(&history.chain_net)?;
    let p = parse32(&history.internal_key, "internal_key")?;
    let genesis_gate = parse_outpoint(&history.genesis_gate_seal)?;

    let issuance = bfa::issue_bfa(
        chain_net,
        &history.name,
        &history.ticker,
        history.max_supply,
        genesis_gate,
        &terms,
    )?;

    eprintln!("──────────────────────────────────────────────────────────");
    eprintln!(" BFA full-history audit — {} mints", history.mints.len());
    eprintln!(" contract : {}", issuance.contract_id);
    eprintln!(" backing  : {}", terms.to_canonical());
    eprintln!("──────────────────────────────────────────────────────────");

    let rpc = spike_env::elements_rpc::ElementsRpc::from_defaults();
    let mut consume_opid = rgbcore::OpId::from(issuance.contract_id.to_byte_array());
    let mut allowance = history.max_supply;
    let mut gate = genesis_gate;
    let mut failures = 0usize;

    for (i, m) in history.mints.iter().enumerate() {
        let n = i + 1;
        let recipient = parse_outpoint(&m.recipient_seal)?;
        let new_gate = m.new_gate_seal.as_deref().map(parse_outpoint).transpose()?;

        // Rebuild the committed transition from the claimed parameters.
        let (bundle_id, transition) = bfa::build_bfa_mint(
            issuance.contract_id,
            consume_opid,
            0,
            allowance,
            m.mint,
            recipient,
            new_gate,
        )?;

        // Recompute the anchor commitment for that transition.
        let entries = vec![mpc::Entry {
            protocol_id: issuance.contract_id.to_byte_array(),
            message: bundle_id.to_byte_array(),
        }];
        let (root, _) = mpc::build(&entries, history.entropy)?;
        let committed = liquid_dbc::commit(p, root)?;

        // Fetch the witness transaction from the chain.
        let raw = rpc
            .call("getrawtransaction", serde_json::json!([m.witness_txid]))
            .await
            .with_context(|| format!("mint #{n}: fetching witness tx {}", m.witness_txid))?;
        let raw_hex = raw
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("getrawtransaction returned non-string"))?;
        let witness_tx: elements::Transaction =
            elements::encode::deserialize(&hex::decode(raw_hex).context("witness tx hex")?)
                .map_err(|e| anyhow::anyhow!("deserialize witness tx: {e}"))?;

        eprintln!(
            " mint #{n}: {} {} (witness {})",
            m.mint, history.ticker, m.witness_txid
        );

        // 1. Seal closure: the witness tx spends the gate seal.
        let closes_gate = witness_tx.input.iter().any(|txin| {
            txin.previous_output.txid.to_string() == gate.txid.to_string()
                && txin.previous_output.vout == gate.vout
        });
        if closes_gate {
            eprintln!(
                "   ✓ gate seal {}:{} closed by witness tx",
                gate.txid, gate.vout
            );
        } else {
            eprintln!(
                "   ✗ SEAL: witness tx does not spend gate seal {}:{}",
                gate.txid, gate.vout
            );
            failures += 1;
        }

        // 2. Anchor: the witness tx pays the recomputed commitment.
        let anchored = witness_tx
            .output
            .iter()
            .any(|o| hex::encode(o.script_pubkey.as_bytes()) == committed.committed_spk_hex);
        if anchored {
            eprintln!("   ✓ anchor commitment matches the rebuilt transition");
        } else {
            eprintln!(
                "   ✗ ANCHOR: no output pays the commitment for this history — \
                 the claimed history does not match what was anchored"
            );
            failures += 1;
        }

        // 3. The backing rule (only meaningful against a matching anchor).
        if anchored {
            match bfa::audit_mint(&terms, &transition, &witness_tx) {
                Ok(a) => eprintln!(
                    "   ✓ backing: minted {} requires {}, vault locked {}",
                    a.minted, a.required, a.locked
                ),
                Err(e) => {
                    eprintln!("   ✗ BACKING: {e}");
                    failures += 1;
                }
            }
        }

        consume_opid = transition.commit_id();
        allowance = allowance
            .checked_sub(m.mint)
            .context("history mints exceed max supply")?;
        gate = match new_gate {
            Some(g) => g,
            None => {
                anyhow::ensure!(
                    i == history.mints.len() - 1 || allowance == 0,
                    "mint #{n} has no new gate seal but allowance remains"
                );
                gate
            }
        };
    }

    eprintln!("──────────────────────────────────────────────────────────");
    if failures > 0 {
        anyhow::bail!("audit FAILED: {failures} check(s) failed");
    }
    println!("audit OK: {} mints, fully backed", history.mints.len());
    Ok(())
}

/// The full backed-mint verification a receiver runs: the standard
/// anchor + seal-closure checks, then the backing rule against the
/// same witness transaction.
async fn verify_backed_mint(
    anchor_path: &std::path::Path,
    vault_spk_hex: &str,
    backing_asset_s: &str,
    required: u64,
) -> Result<()> {
    use spike_rgb_anchor::mint;

    // 1. Anchor, MPC inclusion, and seal closure: identical to `verify`.
    verify(anchor_path).await?;

    // 2. The backing rule, read off the same witness transaction.
    let s = std::fs::read_to_string(anchor_path).context("reading anchor")?;
    let anchor: LiquidAnchor = serde_json::from_str(&s).context("parsing anchor")?;
    let vault_spk = hex::decode(vault_spk_hex).context("vault_spk hex")?;
    let backing_asset: elements::AssetId = backing_asset_s.parse().context("backing asset id")?;

    let rpc = spike_env::elements_rpc::ElementsRpc::from_defaults();
    let raw = rpc
        .call("getrawtransaction", serde_json::json!([anchor.txid]))
        .await?;
    let raw_hex = raw
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("getrawtransaction returned non-string"))?;
    let witness_tx: elements::Transaction =
        elements::encode::deserialize(&hex::decode(raw_hex).context("witness tx hex")?)
            .map_err(|e| anyhow::anyhow!("deserialize witness tx: {e}"))?;

    let locked = mint::verify_backing(&witness_tx, &vault_spk, &backing_asset, required)?;
    println!(
        "✓ backing verified: {locked} units of {} locked to the vault (required {required})",
        &backing_asset_s[..16.min(backing_asset_s.len())]
    );
    Ok(())
}

async fn verify(anchor_path: &std::path::Path) -> Result<()> {
    let s = std::fs::read_to_string(anchor_path).context("reading anchor")?;
    let anchor: LiquidAnchor = serde_json::from_str(&s).context("parsing anchor")?;

    if anchor.txid.is_empty() {
        anyhow::bail!("anchor.txid empty — patch it after broadcast");
    }
    let p = parse32(&anchor.internal_key_hex, "internal_key_hex")?;
    let claimed_root = parse32(&anchor.mpc_root_hex, "mpc_root_hex")?;

    // Rebuild the MPC tree from entries.
    let entries: Vec<mpc::Entry> = anchor
        .entries
        .iter()
        .map(|e| {
            Ok::<_, anyhow::Error>(mpc::Entry {
                protocol_id: parse32(&e.protocol_id_hex, "protocol_id_hex")?,
                message: parse32(&e.message_hex, "message_hex")?,
            })
        })
        .collect::<Result<_>>()?;
    let (root, tree) = mpc::build(&entries, anchor.static_entropy)?;
    if root != claimed_root {
        anyhow::bail!(
            "MPC root mismatch: rebuilt {} != claimed {}",
            hex::encode(root),
            hex::encode(claimed_root)
        );
    }
    tracing::info!(
        "MPC root matches claim ({} entries): {}",
        entries.len(),
        hex::encode(root)
    );

    // Inclusion proof for each entry — confirms the receiver-side flow
    // would work without needing the whole tree.
    for entry in &entries {
        let proof = mpc::inclusion_proof(&tree, entry.protocol_id)?;
        mpc::verify_inclusion(&proof, entry.protocol_id, entry.message, root)?;
        tracing::info!(
            "inclusion proof OK for protocol_id={}",
            &hex::encode(entry.protocol_id)[..16]
        );
    }

    // Recompute expected scriptPubKey via bp-dbc.
    let committed = liquid_dbc::commit(p, root)?;
    let expected_spk = committed.committed_spk_hex;
    tracing::info!("expected SPK = {expected_spk}");

    // Fetch the on-chain witness tx (inputs + outputs).
    let rpc = spike_env::elements_rpc::ElementsRpc::from_defaults();
    let witness = seal::fetch_witness_tx(&rpc, &anchor.txid).await?;

    if let Some(ref s) = anchor.seal {
        // Seal-aware anchor: also verify seal closure.
        let v = seal::verify_seal_closure(&witness, s, &expected_spk)?;
        tracing::info!(
            "seal {}:{} closed at vin[{}]; commitment at vout[{}]",
            s.txid,
            s.vout,
            v.seal_input_index,
            v.commitment_output_index
        );
        // bp-dbc verifier on the SPK bytes (unmodified upstream code).
        let found_spk = &witness.vouts_spk_hex[v.commitment_output_index];
        liquid_dbc::verify(found_spk, root, p)?;
        println!(
            "✓ verified by rgb-consensus + seal closure on Elements:\n  \
             tx {} spends seal {}:{} (vin[{}]) AND commits to MPC root {} (vout[{}])",
            anchor.txid,
            s.txid,
            s.vout,
            v.seal_input_index,
            hex::encode(root),
            v.commitment_output_index
        );
    } else {
        // Commitment-only anchor.
        let idx = witness
            .vouts_spk_hex
            .iter()
            .position(|spk| spk == &expected_spk)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "no matching output in tx {} ({} outputs)",
                    anchor.txid,
                    witness.vouts_spk_hex.len()
                )
            })?;
        liquid_dbc::verify(&witness.vouts_spk_hex[idx], root, p)?;
        println!(
            "✓ verified by rgb-consensus on Elements SPK at vout[{idx}]: tx {} commits to MPC root {}",
            anchor.txid, hex::encode(root)
        );
    }
    Ok(())
}
