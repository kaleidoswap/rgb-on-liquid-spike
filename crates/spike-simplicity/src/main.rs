//! `simp` — drive a SimplicityHL program on Liquid regtest.
//!
//! Two commands:
//!
//! - `address`: compile a `.simf` program (with `param::` arguments),
//!   wrap its CMR in a taproot leaf (version 0xbe) under a NUMS
//!   internal key, and print the address + scriptPubKey + CMR.
//!
//! - `spend`: build the transaction that spends a UTXO locked by the
//!   program, satisfy the program with witness values against the real
//!   transaction environment (so introspection jets see the actual
//!   outputs), prune it, and emit the raw tx hex with the Simplicity
//!   witness stack `[witness, program, leaf-script, control-block]`.
//!
//! The witness-stack layout and environment construction mirror
//! Blockstream's `hal-simplicity` PSET finalizer.

use std::str::FromStr;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use simplicity::jet::elements::{ElementsEnv, ElementsUtxo};
use simplicityhl::ast::ElementsJetHinter;
use simplicityhl::elements;
use simplicityhl::{Arguments, CompiledProgram, TemplateProgram, WitnessValues};

use elements::secp256k1_zkp as secp256k1;

/// BIP-341 NUMS point — no known discrete log, so no key-path spend.
const NUMS_KEY: &str = "50929b74c1a04954b78b4b6035e97a5e078a5a0f28ec96d547bfee9ace803ac0";

/// The RGB `opret` anchor shape the covenant enforces at output 0:
/// `OP_RETURN OP_PUSHBYTES_32 <payload>`.
fn opret_spk(payload: &[u8; 32]) -> Vec<u8> {
    let mut spk = Vec::with_capacity(34);
    spk.push(0x6a); // OP_RETURN
    spk.push(0x20); // OP_PUSHBYTES_32
    spk.extend_from_slice(payload);
    spk
}

#[derive(Parser)]
#[command(about = "Drive a SimplicityHL program on Liquid regtest")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Compile the program and print its taproot address (leaf 0xbe).
    Address {
        /// Path to the .simf program.
        #[arg(long)]
        program: String,
        /// Path to a JSON file with the program's `param::` arguments.
        #[arg(long)]
        args: String,
    },
    /// Build + satisfy the spending transaction. Prints raw tx hex.
    Spend {
        #[arg(long)]
        program: String,
        #[arg(long)]
        args: String,
        /// Path to a JSON file with the program's `witness::` values.
        #[arg(long)]
        witness: String,
        #[arg(long)]
        prev_txid: String,
        #[arg(long)]
        prev_vout: u32,
        #[arg(long)]
        input_value_sat: u64,
        /// Destination scriptPubKey hex.
        #[arg(long)]
        dest_spk: String,
        #[arg(long, default_value_t = 1000)]
        fee_sat: u64,
        /// L-BTC asset id (display hex) on the target chain.
        #[arg(long)]
        lbtc_asset: String,
        /// Genesis block hash of the target chain.
        #[arg(long)]
        genesis_hash: String,
        /// 32-byte payload for an `OP_RETURN OP_PUSHBYTES_32 <payload>`
        /// output placed at vout 0 (the RGB opret anchor).
        #[arg(long)]
        opret_payload: Option<String>,
        /// AFTER satisfying against a compliant transaction, strip the
        /// anchor output and re-serialize (negative test: the stale
        /// witness must fail consensus because the covenant re-runs
        /// on-chain against the real outputs).
        #[arg(long, default_value_t = false)]
        tamper_drop_anchor: bool,
    },
}

fn compile(program_path: &str, args_path: &str) -> Result<CompiledProgram> {
    let src =
        std::fs::read_to_string(program_path).with_context(|| format!("read {program_path}"))?;
    let template = TemplateProgram::new(src, Box::new(ElementsJetHinter::new()))
        .map_err(|e| anyhow::anyhow!("parse: {e}"))?;
    let args: Arguments = serde_json::from_str(
        &std::fs::read_to_string(args_path).with_context(|| format!("read {args_path}"))?,
    )
    .context("parse args JSON")?;
    template
        .instantiate(args, false)
        .map_err(|e| anyhow::anyhow!("instantiate: {e}"))
}

struct TaprootParts {
    address: elements::Address,
    spk: elements::Script,
    leaf_script: elements::Script,
    control_block: elements::taproot::ControlBlock,
    cmr: simplicity::Cmr,
}

fn taproot_parts(compiled: &CompiledProgram) -> Result<TaprootParts> {
    let secp = secp256k1::Secp256k1::new();
    let cmr = compiled.commit().cmr();
    let leaf_script = elements::Script::from(cmr.as_ref().to_vec());
    let internal_key = elements::bitcoin::key::XOnlyPublicKey::from_str(NUMS_KEY)?;

    let spend_info = elements::taproot::TaprootBuilder::new()
        .add_leaf_with_ver(0, leaf_script.clone(), simplicity::leaf_version())
        .map_err(|e| anyhow::anyhow!("taproot builder: {e:?}"))?
        .finalize(&secp, internal_key)
        .map_err(|e| anyhow::anyhow!("taproot finalize: {e:?}"))?;

    let control_block = spend_info
        .control_block(&(leaf_script.clone(), simplicity::leaf_version()))
        .context("control block for simplicity leaf")?;

    let address = elements::Address::p2tr(
        &secp,
        internal_key,
        spend_info.merkle_root(),
        None,
        &elements::AddressParams::ELEMENTS,
    );
    let spk = address.script_pubkey();

    Ok(TaprootParts {
        address,
        spk,
        leaf_script,
        control_block,
        cmr,
    })
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Address { program, args } => {
            let compiled = compile(&program, &args)?;
            let parts = taproot_parts(&compiled)?;
            println!(
                "{}",
                serde_json::json!({
                    "cmr": parts.cmr.to_string(),
                    "address": parts.address.to_string(),
                    "spk_hex": hex::encode(parts.spk.as_bytes()),
                    "leaf_version": "0xbe",
                })
            );
            Ok(())
        }
        Cmd::Spend {
            program,
            args,
            witness,
            prev_txid,
            prev_vout,
            input_value_sat,
            dest_spk,
            fee_sat,
            lbtc_asset,
            genesis_hash,
            opret_payload,
            tamper_drop_anchor,
        } => {
            use elements::confidential::{Asset, Nonce, Value};
            use elements::{
                AssetId, OutPoint, Script, Sequence, Transaction, TxIn, TxInWitness, TxOut,
                TxOutWitness,
            };

            let compiled = compile(&program, &args)?;
            let parts = taproot_parts(&compiled)?;

            let witness_values: WitnessValues = serde_json::from_str(
                &std::fs::read_to_string(&witness).with_context(|| format!("read {witness}"))?,
            )
            .context("parse witness JSON")?;

            let txid: elements::Txid = prev_txid.parse().context("prev_txid")?;
            let asset_id: AssetId = lbtc_asset.parse().context("lbtc asset id")?;
            let lbtc = Asset::Explicit(asset_id);
            let genesis: elements::BlockHash = genesis_hash.parse().context("genesis hash")?;
            let dest = hex::decode(&dest_spk).context("dest_spk hex")?;

            let mut output = Vec::new();
            if let Some(payload_hex) = &opret_payload {
                let payload: [u8; 32] = hex::decode(payload_hex)
                    .context("opret payload hex")?
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("opret payload must be 32 bytes"))?;
                let spk = opret_spk(&payload);
                output.push(TxOut {
                    asset: lbtc,
                    value: Value::Explicit(0),
                    nonce: Nonce::Null,
                    script_pubkey: Script::from(spk),
                    witness: TxOutWitness::default(),
                });
            }
            output.push(TxOut {
                asset: lbtc,
                value: Value::Explicit(
                    input_value_sat
                        .checked_sub(fee_sat)
                        .context("fee exceeds input")?,
                ),
                nonce: Nonce::Null,
                script_pubkey: Script::from(dest),
                witness: TxOutWitness::default(),
            });
            output.push(TxOut {
                asset: lbtc,
                value: Value::Explicit(fee_sat),
                nonce: Nonce::Null,
                script_pubkey: Script::new(),
                witness: TxOutWitness::default(),
            });

            let mut tx = Transaction {
                version: 2,
                lock_time: elements::LockTime::ZERO,
                input: vec![TxIn {
                    previous_output: OutPoint::new(txid, prev_vout),
                    is_pegin: false,
                    script_sig: Script::new(),
                    sequence: Sequence::from_consensus(0xffff_fffd),
                    asset_issuance: Default::default(),
                    witness: TxInWitness::default(),
                }],
                output,
            };

            // The environment the introspection jets run against: the
            // REAL spending transaction.
            let utxo = ElementsUtxo {
                script_pubkey: parts.spk.clone(),
                asset: lbtc,
                value: Value::Explicit(input_value_sat),
            };
            let env = ElementsEnv::new(
                Arc::new(tx.clone()),
                vec![utxo],
                0,
                parts.cmr,
                parts.control_block.clone(),
                None,
                genesis,
            );

            // Satisfy + prune. If the program's assertions fail against
            // this transaction (e.g. wrong preimage), this fails HERE —
            // pass witness values that satisfy the compliant layout.
            let satisfied = compiled
                .satisfy_with_env(witness_values, Some(&env))
                .map_err(|e| anyhow::anyhow!("satisfy: {e}"))?;
            let (prog_bytes, wit_bytes) = satisfied.redeem().to_vec_with_witness();

            if tamper_drop_anchor {
                // Strip vout 0 (the anchor) AFTER satisfaction: the
                // witness was produced for the compliant tx; consensus
                // re-executes against this mutated one and must reject.
                anyhow::ensure!(
                    opret_payload.is_some(),
                    "--tamper-drop-anchor needs --opret-payload"
                );
                tx.output.remove(0);
            }

            tx.input[0].witness.script_witness = vec![
                wit_bytes,
                prog_bytes,
                parts.leaf_script.clone().into_bytes(),
                parts.control_block.serialize(),
            ];

            println!("{}", hex::encode(elements::encode::serialize(&tx)));
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH_A: &str = "66687aadf862bd776c8fc18b8e9f8e20089714856ee233b3902a591d0d5f2925";
    const HASH_B: &str = "0000000000000000000000000000000000000000000000000000000000000001";

    fn program_path() -> String {
        format!(
            "{}/programs/rgb_anchor_covenant.simf",
            env!("CARGO_MANIFEST_DIR")
        )
    }

    fn compile_with_hash(hash: &str) -> CompiledProgram {
        let args = format!(r#"{{ "EXPECTED_HASH": {{ "value": "0x{hash}", "type": "u256" }} }}"#);
        let dir = std::env::temp_dir().join(format!("simp-test-args-{hash}.json"));
        std::fs::write(&dir, args).unwrap();
        compile(&program_path(), dir.to_str().unwrap()).expect("covenant program compiles")
    }

    #[test]
    fn opret_spk_is_anchor_shaped() {
        let payload = [0xabu8; 32];
        let spk = opret_spk(&payload);
        assert_eq!(spk.len(), 34);
        assert_eq!(spk[0], 0x6a, "OP_RETURN");
        assert_eq!(spk[1], 0x20, "OP_PUSHBYTES_32");
        assert_eq!(&spk[2..], &payload);
    }

    #[test]
    fn bundled_covenant_compiles_and_cmr_is_deterministic() {
        let a1 = compile_with_hash(HASH_A);
        let a2 = compile_with_hash(HASH_A);
        assert_eq!(
            a1.commit().cmr(),
            a2.commit().cmr(),
            "same program + same argument must give the same CMR"
        );
    }

    #[test]
    fn hash_argument_is_baked_into_the_cmr() {
        let a = compile_with_hash(HASH_A);
        let b = compile_with_hash(HASH_B);
        assert_ne!(
            a.commit().cmr(),
            b.commit().cmr(),
            "a different hashlock must change the CMR (and thus the address)"
        );
    }

    #[test]
    fn taproot_parts_are_wellformed() {
        let parts = taproot_parts(&compile_with_hash(HASH_A)).unwrap();
        // 32-byte CMR is the whole leaf script
        assert_eq!(parts.leaf_script.as_bytes(), parts.cmr.as_ref());
        // P2TR scriptPubKey: OP_1 PUSH32 <output key>
        let spk = parts.spk.as_bytes();
        assert_eq!(spk.len(), 34);
        assert_eq!(spk[0], 0x51);
        assert_eq!(spk[1], 0x20);
        // single-leaf control block: 1 version+parity byte + 32-byte internal key
        assert_eq!(parts.control_block.serialize().len(), 33);
        // simplicity tapleaf version
        assert_eq!(parts.control_block.leaf_version.as_u8(), 0xbe);
        // regtest HRP
        assert!(parts.address.to_string().starts_with("ert1p"));
    }

    #[test]
    fn addresses_differ_per_hashlock() {
        let a = taproot_parts(&compile_with_hash(HASH_A)).unwrap();
        let b = taproot_parts(&compile_with_hash(HASH_B)).unwrap();
        assert_ne!(a.address.to_string(), b.address.to_string());
    }
}
