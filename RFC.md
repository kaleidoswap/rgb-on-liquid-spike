# RFC: a `WitnessTx` abstraction to unblock RGB on Liquid

**Target:** [`rgb-protocol/rgb-consensus`](https://github.com/rgb-protocol/rgb-consensus) (`rgbcore` v0.11.1-rc.10)
**From:** KaleidoSwap engineering
**Status:** Draft for maintainer review. The patch is implemented and the upstream test suite passes; it is staged as a pull request on a fork and has not been submitted upstream.

## The ask

RGB consensus already names Liquid: `Layer1::Liquid`, `ChainNet::LiquidMainnet/LiquidTestnet`, and a `chain_net` field on `Genesis`. The crypto layer (`mpc`, `dbc::tapret`, `dbc::opret`) is already chain-agnostic — a P2TR `scriptPubKey` is byte-identical on Bitcoin and Elements.

The one thing that still blocks RGB-on-Liquid is that the witness-verification path is typed against `bitcoin::Transaction`, which cannot deserialize an Elements tx. We propose abstracting that one type behind a small `WitnessTx` trait. We have written the patch and validated it; details below.

## The gap

The Bitcoin-tx coupling in the verification path is exactly three spots:

```rust
// 1. dbc/proof.rs
fn verify(&self, msg: &mpc::Commitment, tx: &bitcoin::Transaction) -> Result<(), Self::Error>;

// 2. dbc/anchor.rs
impl<D: dbc::Proof> Anchor<D> {
    pub fn verify(&self, protocol_id: ..., message: ..., tx: &bitcoin::Transaction) -> Result<...>;
}

// 3. seals/txout/witness.rs
pub struct Witness<D: dbc::Proof> { pub tx: bitcoin::Transaction, /* ... */ }
```

Everything those paths need from `tx` is small: the input outpoints (seal-closure check) and the output scriptPubKeys (tapret/opret recovery). No witness data, no fees, no signatures.

## Proposed change

A three-method trait, implemented for `bitcoin::Transaction` so existing callers are unaffected:

```rust
pub trait WitnessTx {
    fn witness_txid(&self) -> [u8; 32];
    fn input_outpoints(&self) -> Vec<([u8; 32], u32)>;
    fn output_script_pubkeys(&self) -> Vec<Vec<u8>>;
}
```

`dbc::Proof::verify` and `Anchor::verify` become generic over `W: WitnessTx`. The concrete `TapretProof` / `OpretProof` impls iterate `output_script_pubkeys()` and dispatch to the existing `ScriptBuf`-level `ConvolveCommitProof` / `EmbedCommitVerify` paths, which were already chain-agnostic. Any Elements/Liquid tx type then participates by implementing three methods.

## Evidence

We applied the patch to a vendored `rgb-consensus` 0.11.1-rc.10 and ran it end-to-end on Liquid regtest.

| Result | |
|---|---|
| Patch size | 207 LOC, 7 files (+1 new) |
| Upstream test suite on the patched crate | **45 / 45 pass** |
| Breaking changes for `bitcoin::Transaction` callers | none |
| `rgb-ops`, `rgb-schemas`, `rgb-invoicing`, `rgb-aluvm` | compile + run **unchanged** |
| Liquid-side adapter (`impl WitnessTx`) | ~30 LOC |

The patched `Anchor::verify` accepts both a Bitcoin and a Liquid witness tx through the same call. A real RGB20 (NIA) contract, issued and transferred via the unmodified `rgb-ops` + `rgb-schemas`, anchors and verifies on Liquid. A cross-chain RGB atomic swap (Bitcoin ↔ Liquid) settles with both legs verified by the patched verifier.

Per-file diff: `dbc/witness_tx.rs` (+58, new), `dbc/proof.rs` (±20), `dbc/anchor.rs` (±18), `dbc/tapret/mod.rs` (±32), `dbc/opret/mod.rs` (±29), `seals/txout/witness.rs` (±72), `dbc/mod.rs` (+4), `validation/commitments.rs` (±6).

## Compatibility

`bitcoin::Transaction` implements `WitnessTx`, so `anchor.verify(pid, msg, &bitcoin_tx)` compiles and behaves identically. The only surface change is `dbc::Proof::verify` gaining a generic parameter — a minor-version bump, no semantic change.

One integration note worth documenting: the `TapretFirst` convention requires the commitment to be the first P2TR output. A Liquid wallet that auto-sorts outputs will silently break verification.

## Open questions

1. **`WitnessTx` location** — inside `rgb-consensus` (our preference) or a sibling crate?
2. **Trait shape** — are the three methods the right minimum, or should `version` / `locktime` be exposed for future proofs?
3. **`Witness<D>` struct** — make its `tx` field generic (`Witness<D, T = bitcoin::Transaction>`), or add a `WitnessGeneric<D, T>` sibling?
4. **RGB-WG 0.12 line** — `bp-dbc` / `bp-core` has the identical coupling in the same spot. Worth coordinating?

## What we will do

KaleidoSwap will write the patch against whichever shape you prefer, maintain a `WitnessTx` impl for `elements::Transaction` in a sibling repo so integrators have a drop-in path, and ship regression tests plus the working demo with the PR. We are happy to land it first and integrate downstream later.

## Reproducer

[`kaleidoswap/rgb-on-liquid-spike`](https://github.com/kaleidoswap/rgb-on-liquid-spike) — `vendor/rgb-consensus-patched/` holds the patch; `PATCH.md` there lists the file-by-file change.

```
docker compose up -d
./scripts/bootstrap.sh && ./scripts/bootstrap_btc.sh
./scripts/demo_rgb20.sh    # real RGB20 transfer on Liquid
./scripts/demo_swap.sh     # cross-chain Bitcoin <-> Liquid atomic swap
cargo test -p rgb-consensus --features rand   # 45/45 upstream tests pass
```
