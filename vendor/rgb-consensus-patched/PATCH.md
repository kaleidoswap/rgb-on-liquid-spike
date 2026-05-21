# Vendored, patched `rgb-consensus`

This directory is a vendored copy of
[`rgb-protocol/rgb-consensus`](https://github.com/rgb-protocol/rgb-consensus)
**v0.11.1-rc.10** (Apache-2.0), with one change applied: the `WitnessTx`
abstraction that lets the witness-verification path accept a non-Bitcoin
transaction.

It is renamed to `rgb-consensus-patched` only in package metadata; the
library name stays `rgbcore`, and the workspace's `[patch.crates-io]`
redirects `rgb-consensus` here so the rest of the dependency tree
transitively picks up the patch.

The patch is the subject of [`../../docs/RFC_RGB_ON_LIQUID.md`](../../docs/RFC_RGB_ON_LIQUID.md).
It touches eight files:

- `src/dbc/witness_tx.rs` *(new)* — the `WitnessTx` trait + an impl for
  `bitcoin::Transaction`.
- `src/dbc/mod.rs` — re-export `WitnessTx`.
- `src/dbc/proof.rs` — `Proof::verify` becomes generic over `W: WitnessTx`.
- `src/dbc/anchor.rs` — `Anchor::verify` becomes generic.
- `src/dbc/tapret/mod.rs`, `src/dbc/opret/mod.rs` — the concrete `Proof`
  impls iterate `output_script_pubkeys()`.
- `src/seals/txout/witness.rs` — `verify_seal` delegates to a shared,
  `WitnessTx`-generic helper.
- `src/validation/commitments.rs` — `DbcProof::verify` becomes generic.

All other files are upstream's, unchanged. Upstream's `LICENSE`,
`CHANGELOG.md`, and `README.md` are retained.
