//! `tapret` — CLI for naked tapret commitments on Liquid.
//!
//! Subcommands:
//!
//!   tweak    — compute Q from (P, C)
//!   address  — encode Q as a bech32m taproot address
//!   verify   — given a txid, recompute Q from (P, C) and confirm a match
//!
//! Broadcast is left to the node CLI (`elements-cli sendtoaddress`);
//! this tool computes the address and verifies the result on-chain.

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use spike_tapret::{address, tweak};

#[derive(Parser)]
#[command(name = "tapret", version, about = "Naked tapret commitment on Liquid")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Compute Q = P + H_taptweak(P||C)·G and print Q as hex.
    Tweak {
        #[arg(long)]
        internal_key: String,
        #[arg(long)]
        commitment: String,
    },
    /// Compute Q and print a Liquid regtest P2TR address (ert1p...).
    Address {
        #[arg(long)]
        internal_key: String,
        #[arg(long)]
        commitment: String,
        #[arg(long, default_value = "ert")]
        hrp: String,
    },
    /// Fetch tx from elementsd RPC, find a P2TR output paying to the
    /// expected key, and verify the commitment.
    Verify {
        #[arg(long)]
        txid: String,
        #[arg(long)]
        internal_key: String,
        #[arg(long)]
        commitment: String,
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

    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Tweak {
            internal_key,
            commitment,
        } => {
            let (p, c) = parse_pc(&internal_key, &commitment)?;
            let tk = tweak::prove(&p, &c)?;
            println!("{}", hex::encode(tk.output));
        }
        Cmd::Address {
            internal_key,
            commitment,
            hrp,
        } => {
            let (p, c) = parse_pc(&internal_key, &commitment)?;
            let tk = tweak::prove(&p, &c)?;
            let addr = address::encode_p2tr(&hrp, &tk.output)?;
            println!("{addr}");
        }
        Cmd::Verify {
            txid,
            internal_key,
            commitment,
        } => {
            let (p, c) = parse_pc(&internal_key, &commitment)?;
            verify_onchain(&txid, &p, &c).await?;
            println!(
                "✓ verified: tx {txid} commits to ({}, {})",
                short(&internal_key),
                short(&commitment)
            );
        }
    }
    Ok(())
}

fn parse_pc(p_hex: &str, c_hex: &str) -> Result<([u8; 32], [u8; 32])> {
    let p = hex::decode(p_hex).context("internal_key is not valid hex")?;
    let c = hex::decode(c_hex).context("commitment is not valid hex")?;
    if p.len() != 32 {
        anyhow::bail!("internal_key must be 32 bytes ({} given)", p.len());
    }
    if c.len() != 32 {
        anyhow::bail!("commitment must be 32 bytes ({} given)", c.len());
    }
    let mut pp = [0u8; 32];
    let mut cc = [0u8; 32];
    pp.copy_from_slice(&p);
    cc.copy_from_slice(&c);
    Ok((pp, cc))
}

fn short(s: &str) -> String {
    if s.len() > 12 {
        format!("{}…{}", &s[..6], &s[s.len() - 6..])
    } else {
        s.to_owned()
    }
}

/// Fetch tx via elementsd RPC and look for a P2TR output whose witness
/// program equals `prove(p, c).output`.
async fn verify_onchain(txid: &str, p: &[u8; 32], c: &[u8; 32]) -> Result<()> {
    let rpc = spike_env::elements_rpc::ElementsRpc::from_defaults();

    // getrawtransaction with verbosity=2 returns a fully-decoded tx.
    let v = rpc
        .call("getrawtransaction", serde_json::json!([txid, 2]))
        .await
        .context("getrawtransaction failed")?;

    let vouts = v["vout"]
        .as_array()
        .ok_or_else(|| anyhow!("no vout array in tx"))?;

    let expected = tweak::prove(p, c)?;
    let expected_hex = hex::encode(expected.output);
    let expected_spk_hex = format!("5120{expected_hex}"); // OP_1 PUSH32 <Q>

    tracing::info!(target: "verify", "expected Q = {}", expected_hex);
    tracing::info!(target: "verify", "expected scriptPubKey hex = {}", expected_spk_hex);

    let mut found = None;
    for (i, out) in vouts.iter().enumerate() {
        let spk = out["scriptPubKey"]["hex"]
            .as_str()
            .unwrap_or("")
            .to_lowercase();
        tracing::debug!(target: "verify", "  vout[{i}] scriptPubKey = {spk}");
        if spk == expected_spk_hex {
            found = Some((i, out));
            break;
        }
    }

    let (idx, _out) = found.ok_or_else(|| {
        anyhow!(
            "no output matches the expected P2TR scriptPubKey ({expected_spk_hex}); \
             scanned {} outputs",
            vouts.len()
        )
    })?;

    tracing::info!(target: "verify", "found commitment at vout[{idx}]");

    // Cross-check against the tweak module's own verify().
    tweak::verify(p, c, &expected.output)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pc_rejects_short() {
        assert!(parse_pc("ab", "cd").is_err());
    }
}
