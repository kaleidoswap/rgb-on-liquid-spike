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
    internal_key_hex: &str,
    entropy: u64,
    chain_net_s: &str,
) -> Result<()> {
    let p = parse32(internal_key_hex, "internal_key")?;
    let alice_seal = parse_outpoint(alice_seal_s)?;
    let bob_seal = parse_outpoint(bob_seal_s)?;
    let change_seal = change_seal_s.map(parse_outpoint).transpose()?;
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

    // Step 2: real transfer transition.
    let (bundle_id, transition) =
        rgb20::build_transfer(issuance.contract_id, supply, send, bob_seal, change_seal, 0)?;
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
        txid: format!("{}", alice_seal.txid),
        vout: alice_seal.vout,
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
