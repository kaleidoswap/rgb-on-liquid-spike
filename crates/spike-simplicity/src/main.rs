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

/// Deterministic demo keypair from a label: `sk = SHA256(label)`.
/// Regtest-demo convenience only.
fn demo_keypair(label: &str) -> Result<(secp256k1::SecretKey, secp256k1::PublicKey)> {
    use elements::hashes::{sha256, Hash};
    let secp = secp256k1::Secp256k1::new();
    let sk_bytes = sha256::Hash::hash(label.as_bytes());
    let sk = secp256k1::SecretKey::from_slice(sk_bytes.as_ref())
        .context("label hashed to an invalid secret key")?;
    let pk = secp256k1::PublicKey::from_secret_key(&secp, &sk);
    Ok((sk, pk))
}

/// P2WPKH scriptPubKey for a demo key: `OP_0 PUSH20 hash160(pubkey)`.
fn p2wpkh_spk(pk: &secp256k1::PublicKey) -> Vec<u8> {
    use elements::hashes::{hash160, Hash};
    let h = hash160::Hash::hash(&pk.serialize());
    let mut spk = Vec::with_capacity(22);
    spk.push(0x00);
    spk.push(0x14);
    spk.extend_from_slice(h.as_ref());
    spk
}

/// BIP-143 script_code for a P2WPKH input: the classic P2PKH script.
fn p2wpkh_script_code(pk: &secp256k1::PublicKey) -> Vec<u8> {
    use elements::hashes::{hash160, Hash};
    let h = hash160::Hash::hash(&pk.serialize());
    let mut sc = Vec::with_capacity(25);
    sc.extend_from_slice(&[0x76, 0xa9, 0x14]);
    sc.extend_from_slice(h.as_ref());
    sc.extend_from_slice(&[0x88, 0xac]);
    sc
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
    /// Print the P2WPKH funding address for a demo key label.
    DemoAddress {
        #[arg(long)]
        label: String,
    },
    /// Spend a mint-gate covenant UTXO: build the backed-mint witness
    /// transaction (anchor at 0, vault tranche at 1, recipient seal at
    /// 2, recursive gate at 3), satisfy the covenant against it, sign
    /// the demo-key funding inputs, and print the raw tx hex.
    MintSpend {
        #[arg(long)]
        program: String,
        #[arg(long)]
        args: String,
        /// 32-byte MPC root committed by the opret anchor at output 0.
        #[arg(long)]
        anchor_payload: String,
        /// Gate covenant UTXO being spent (vin 0).
        #[arg(long)]
        gate_txid: String,
        #[arg(long)]
        gate_vout: u32,
        #[arg(long)]
        gate_value_sat: u64,
        /// Backing-asset UTXO holding exactly the tranche (vin 1),
        /// paying the demo key's P2WPKH.
        #[arg(long)]
        asset_txid: String,
        #[arg(long)]
        asset_vout: u32,
        /// L-BTC fee UTXO (vin 2), paying the demo key's P2WPKH.
        #[arg(long)]
        fee_txid: String,
        #[arg(long)]
        fee_vout: u32,
        #[arg(long)]
        fee_input_sat: u64,
        /// Demo key label owning vins 1 and 2 and the change.
        #[arg(long, default_value = "minter")]
        key_label: String,
        /// Vault scriptPubKey hex (must hash to the covenant's
        /// VAULT_SPK_HASH argument).
        #[arg(long)]
        vault_spk: String,
        /// Backing asset id (display hex) and exact tranche.
        #[arg(long)]
        backing_asset: String,
        #[arg(long)]
        tranche: u64,
        /// Recipient seal scriptPubKey hex (output 2).
        #[arg(long)]
        recipient_spk: String,
        #[arg(long, default_value_t = 30_000)]
        recipient_sat: u64,
        #[arg(long, default_value_t = 1_000)]
        fee_sat: u64,
        #[arg(long)]
        lbtc_asset: String,
        #[arg(long)]
        genesis_hash: String,
        /// Consensus-negative tampering, applied AFTER covenant
        /// satisfaction (the ECDSA inputs are signed over the mutated
        /// tx, so only the covenant can object):
        /// `none`, `drop-anchor`, `wrong-amount`, `no-recreate`.
        #[arg(long, default_value = "none")]
        tamper: String,
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
        Cmd::DemoAddress { label } => {
            let (_, pk) = demo_keypair(&label)?;
            let btc_pk = elements::bitcoin::PublicKey::new(pk);
            let addr = elements::Address::p2wpkh(&btc_pk, None, &elements::AddressParams::ELEMENTS);
            println!(
                "{}",
                serde_json::json!({
                    "address": addr.to_string(),
                    "spk_hex": hex::encode(p2wpkh_spk(&pk)),
                })
            );
            Ok(())
        }
        Cmd::MintSpend {
            program,
            args,
            anchor_payload,
            gate_txid,
            gate_vout,
            gate_value_sat,
            asset_txid,
            asset_vout,
            fee_txid,
            fee_vout,
            fee_input_sat,
            key_label,
            vault_spk,
            backing_asset,
            tranche,
            recipient_spk,
            recipient_sat,
            fee_sat,
            lbtc_asset,
            genesis_hash,
            tamper,
        } => {
            use elements::confidential::{Asset, Nonce, Value};
            use elements::hashes::Hash as _;
            use elements::sighash::SighashCache;
            use elements::{
                AssetId, EcdsaSighashType, OutPoint, Script, Sequence, Transaction, TxIn,
                TxInWitness, TxOut, TxOutWitness,
            };

            let compiled = compile(&program, &args)?;
            let parts = taproot_parts(&compiled)?;
            let (sk, pk) = demo_keypair(&key_label)?;
            let funding_spk = Script::from(p2wpkh_spk(&pk));

            let payload = hex::decode(&anchor_payload).context("anchor payload hex")?;
            anyhow::ensure!(payload.len() == 32, "anchor payload must be 32 bytes");
            let mut opret = Vec::with_capacity(34);
            opret.push(0x6a);
            opret.push(0x20);
            opret.extend_from_slice(&payload);

            let lbtc: AssetId = lbtc_asset.parse().context("lbtc asset id")?;
            let backing: AssetId = backing_asset.parse().context("backing asset id")?;
            let genesis: elements::BlockHash = genesis_hash.parse().context("genesis hash")?;
            let vault = hex::decode(&vault_spk).context("vault spk hex")?;
            let recipient = hex::decode(&recipient_spk).context("recipient spk hex")?;

            let change_sat = fee_input_sat
                .checked_sub(recipient_sat + fee_sat)
                .context("fee input too small for recipient + fee")?;

            let mk_in = |txid_s: &str, vout: u32| -> Result<TxIn> {
                Ok(TxIn {
                    previous_output: OutPoint::new(txid_s.parse()?, vout),
                    is_pegin: false,
                    script_sig: Script::new(),
                    sequence: Sequence::from_consensus(0xffff_fffd),
                    asset_issuance: Default::default(),
                    witness: TxInWitness::default(),
                })
            };
            let out = |asset: AssetId, sat: u64, spk: Script| TxOut {
                asset: Asset::Explicit(asset),
                value: Value::Explicit(sat),
                nonce: Nonce::Null,
                script_pubkey: spk,
                witness: TxOutWitness::default(),
            };

            let mut tx = Transaction {
                version: 2,
                lock_time: elements::LockTime::ZERO,
                input: vec![
                    mk_in(&gate_txid, gate_vout)?,
                    mk_in(&asset_txid, asset_vout)?,
                    mk_in(&fee_txid, fee_vout)?,
                ],
                output: vec![
                    out(lbtc, 0, Script::from(opret)),                 // 0: anchor
                    out(backing, tranche, Script::from(vault)),        // 1: vault
                    out(lbtc, recipient_sat, Script::from(recipient)), // 2: recipient seal
                    out(lbtc, gate_value_sat, parts.spk.clone()),      // 3: next gate
                    out(lbtc, change_sat, funding_spk.clone()),        // 4: change
                    out(lbtc, fee_sat, Script::new()),                 // 5: fee
                ],
            };

            // Satisfy the covenant against the COMPLIANT transaction.
            let utxos = vec![
                ElementsUtxo {
                    script_pubkey: parts.spk.clone(),
                    asset: Asset::Explicit(lbtc),
                    value: Value::Explicit(gate_value_sat),
                },
                ElementsUtxo {
                    script_pubkey: funding_spk.clone(),
                    asset: Asset::Explicit(backing),
                    value: Value::Explicit(tranche),
                },
                ElementsUtxo {
                    script_pubkey: funding_spk.clone(),
                    asset: Asset::Explicit(lbtc),
                    value: Value::Explicit(fee_input_sat),
                },
            ];
            let env = ElementsEnv::new(
                Arc::new(tx.clone()),
                utxos,
                0,
                parts.cmr,
                parts.control_block.clone(),
                None,
                genesis,
            );
            let witness_values: WitnessValues = serde_json::from_str(&format!(
                r#"{{ "ANCHOR_PAYLOAD": {{ "value": "0x{anchor_payload}", "type": "u256" }} }}"#
            ))
            .context("witness values")?;
            let satisfied = compiled
                .satisfy_with_env(witness_values, Some(&env))
                .map_err(|e| anyhow::anyhow!("satisfy: {e}"))?;
            let (prog_bytes, wit_bytes) = satisfied.redeem().to_vec_with_witness();

            // Consensus-negative tampering happens AFTER satisfaction
            // and BEFORE ECDSA signing: the funding signatures stay
            // valid, so the only thing that can reject the mutated tx
            // is the covenant itself, inside consensus.
            match tamper.as_str() {
                "none" => {}
                "drop-anchor" => tx.output[0].script_pubkey = funding_spk.clone(),
                "no-recreate" => tx.output[3].script_pubkey = funding_spk.clone(),
                "wrong-amount" => {
                    tx.output[1].value = Value::Explicit(tranche - 1);
                    tx.output.push(out(backing, 1, funding_spk.clone()));
                }
                other => anyhow::bail!("unknown tamper mode: {other}"),
            }

            tx.input[0].witness.script_witness = vec![
                wit_bytes,
                prog_bytes,
                parts.leaf_script.clone().into_bytes(),
                parts.control_block.serialize(),
            ];

            // Sign the demo-key P2WPKH funding inputs over the final tx.
            let secp = secp256k1::Secp256k1::new();
            let script_code = Script::from(p2wpkh_script_code(&pk));
            let mut witnesses = Vec::new();
            for (index, value_sat) in [(1usize, tranche), (2usize, fee_input_sat)] {
                let sighash = SighashCache::new(&tx).segwitv0_sighash(
                    index,
                    &script_code,
                    Value::Explicit(value_sat),
                    EcdsaSighashType::All,
                );
                let msg = secp256k1::Message::from_digest(sighash.to_byte_array());
                let mut sig = secp.sign_ecdsa(&msg, &sk).serialize_der().to_vec();
                sig.push(EcdsaSighashType::All as u8);
                witnesses.push((index, vec![sig, pk.serialize().to_vec()]));
            }
            for (index, w) in witnesses {
                tx.input[index].witness.script_witness = w;
            }

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
