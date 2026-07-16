# rgb-on-liquid-spike

Proof of concept: RGB assets running natively on the Liquid sidechain.

RGB is client-side-validated smart contracts for Bitcoin. Its anchor layer —
the part that binds asset transfers to real on-chain transactions — is typed
against `bitcoin::Transaction`, and that is the only thing that stops RGB from
running on Liquid. This repository shows the gap is a 207-line, non-breaking
change to one library (`rgb-consensus`), and exercises the result end to end
on Bitcoin and Liquid regtest.

The proposed upstream change is written up in [RFC.md](RFC.md).

## What it demonstrates

- A real RGB20 (NIA) asset issued and transferred on Liquid, anchored in a
  Liquid transaction and verified by the patched `rgb-consensus`.
- The same transfer with **confidential outputs**: the closed seal, the new
  seal, and the change are all blinded, and the unchanged verifiers still
  recover and verify the commitment (the commitment lives in the
  scriptPubKey, which Elements never blinds).
- A cross-chain atomic swap: a real RGB20 asset on Bitcoin and one on Liquid,
  exchanged through a shared hashlock, with no custodian.
- A **full HTLC** (claimer key + CSV refund branch) exercised on both chains
  with the same witness-script bytes — claim, wrong-preimage rejection,
  early-refund rejection, and post-timeout refund.
- An **RGB-wrapped HTLC claim**: the HTLC UTXO *is* the RGB seal, so one
  transaction reveals the swap preimage, closes the seal, and carries the
  tapret anchor that re-seats the asset on the claimer's own output.
- **Backed minting**: a real IFA (multi-mint) contract whose supply starts
  as an inflation allowance on a gate seal. Each mint's witness transaction
  must lock a native Elements asset into a vault, one unit per minted unit,
  and every holder verifies that backing straight from the witness tx.
  Unbacked and under-backed mints are rejected (`demo_backed_mint.sh`).
- A **Simplicity covenant** on Liquid regtest (Elements 23.3, tapleaf 0xbe):
  a seal UTXO that consensus only lets you spend by revealing a preimage
  AND carrying an RGB-`opret`-shaped commitment output at vout 0 — the
  `preimage(H) ∧ valid_rgb_anchor` construction, in running code. A spend
  stripped of its anchor output is rejected by the chain itself, not by
  tooling.
- A **Simplicity mint-gate covenant**: permissionless backed minting. The
  gate seal carrying an IFA contract's inflation allowance is locked under a
  covenant, so anyone may mint, but only in a transaction that anchors the
  RGB transition, locks the exact backing tranche to the vault (asset +
  amount introspection jets), and re-creates the gate under the same
  covenant for the next minter (recursion). Two chained mints settle; spends
  that drop the anchor, short the backing, or skip the gate re-creation are
  rejected by consensus (`demo_mint_gate.sh`). A **burn variant**
  (`demo_mint_gate_burn.sh`) replaces the vault with a provable burn: a single
  OP_RETURN output is both the RGB anchor and the destruction of the backing
  tranche, so there is no vault and no key, at the cost of redeemability.
- The patch itself: `rgb-consensus` 0.11.1-rc.10 plus a `WitnessTx` trait,
  vendored under `vendor/rgb-consensus-patched/`. The upstream test suite
  passes unchanged (45/45), and `rgb-ops`, `rgb-schemas`, `rgb-invoicing`, and
  `rgb-aluvm` all compile against the patched crate with no source changes.

## Layout

```
.
├── RFC.md                      the proposed upstream change
├── docker-compose.yml          elementsd (23.2.4 + 23.3.0/Simplicity) + bitcoind regtest
├── docker/elements.conf        Liquid regtest config
├── docker/elements-simplicity.conf  Elements 23.3 config, Simplicity active
├── scripts/                    regtest bootstrap, end-to-end demos, teardown
├── crates/
│   ├── spike-env/              minimal JSON-RPC client for elementsd / bitcoind
│   ├── spike-tapret/           BIP-341 tweak math + P2TR address encoding
│   ├── spike-rgb-anchor/       MPC tree, tapret commit/verify, RGB20, swap + HTLC
│   └── spike-simplicity/       SimplicityHL covenants (anchor gate, mint gate) + driver
└── vendor/
    └── rgb-consensus-patched/  rgb-consensus 0.11.1-rc.10 + the WitnessTx patch
```

## Requirements

- Docker and Docker Compose
- Rust, stable toolchain (1.85 or newer)
- `jq`, `openssl`, `bash`

## Running it

```bash
docker compose up -d            # start elementsd + bitcoind regtest
./scripts/bootstrap.sh          # fund Liquid wallets, issue a test asset
./scripts/bootstrap_btc.sh      # fund the Bitcoin wallet

./scripts/demo_rgb20.sh         # RGB20 issuance + transfer on Liquid
./scripts/demo_confidential.sh  # the same, with blinded (confidential) outputs
./scripts/demo_swap.sh          # cross-chain Bitcoin <-> Liquid atomic swap
./scripts/demo_htlc.sh          # full HTLC (claim + CSV refund) on both chains
./scripts/demo_htlc_rgb.sh      # RGB-wrapped HTLC claim (seal = HTLC UTXO)
./scripts/demo_backed_mint.sh   # IFA mint backed by a locked Elements asset
./scripts/demo_simplicity.sh    # Simplicity covenant: preimage ∧ anchor-shaped output
./scripts/demo_mint_gate.sh     # Simplicity mint-gate (lock): permissionless backed minting
./scripts/demo_mint_gate_burn.sh # Simplicity mint-gate (burn): mint against a provable burn

./scripts/teardown.sh           # stop the nodes and wipe state
```

Two narrower demos are also available: `./scripts/demo_rgb.sh` (a multi-entry
MPC anchor) and `./scripts/demo_seal.sh` (seal-closure verification, with
negative tests).

Note: the demos accumulate wallet state; for a clean run start from
`docker compose down -v` and re-bootstrap.

## Tests

```bash
cargo test --workspace                         # the proof-of-concept crates
cargo test -p rgb-consensus --features rand    # 45 upstream rgb-consensus tests, unchanged
```

## The patch

`vendor/rgb-consensus-patched/` is `rgb-consensus` 0.11.1-rc.10 with the
`WitnessTx` abstraction applied. See
[`vendor/rgb-consensus-patched/PATCH.md`](vendor/rgb-consensus-patched/PATCH.md)
for the file-by-file change, and [RFC.md](RFC.md) for the rationale and the
open questions for upstream maintainers.

## The Simplicity covenant

`crates/spike-simplicity/programs/rgb_anchor_covenant.simf` is, to our
knowledge, the first combination of client-side-validation anchoring with a
Simplicity covenant on the seal UTXO. The program (SimplicityHL, deployed as
a taproot leaf with version 0xbe) enforces two conditions on any spend:

1. `SHA256(witness::PREIMAGE) == param::EXPECTED_HASH` — the atomic-swap
   hashlock, with the hash baked into the CMR and therefore the address;
2. output 0 of the spending transaction has a scriptPubKey of exactly
   `OP_RETURN OP_PUSHBYTES_32 <32 bytes>` — the shape of an RGB `opret`
   commitment. The spender supplies the 32-byte payload (the MPC root) as
   witness; the program reconstructs the expected scriptPubKey hash and
   compares it against `jet::output_script_hash(0)`.

Script cannot see sibling outputs; Simplicity's introspection jets can. The
demo proves enforcement is consensus-level: a transaction satisfied against
a compliant layout and then stripped of its anchor output is rejected by the
node. The claim key/refund branch of a production HTLC is deliberately left
out of the spike program; `demo_htlc.sh` covers that shape in Script.

## Next steps

This is a proof of concept on regtest, not a product. Shipping it takes:

- **Upstreaming the patch** to `rgb-protocol/rgb-consensus`.
- **A Liquid resolver** so a wallet can fetch Liquid witness transactions
  during consignment validation — an adapter over LWK or an Elements-aware
  Esplora.
- **A Liquid backend for `rgb-lib`**, exposing the same issue / send / receive
  API the Bitcoin path already provides.
- **A swap coordinator** around the HTLC + RGB-wrapped-claim flow that this
  repo now demonstrates end to end (timeout selection, refund monitoring,
  consignment exchange).
- **Hardening the Simplicity covenant** into the full swap program (claimer
  signature, CSV refund branch, tapret support) as the SimplicityHL
  toolchain matures.

None of these are research problems.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your
option.

`vendor/rgb-consensus-patched/` is a derivative of
[`rgb-protocol/rgb-consensus`](https://github.com/rgb-protocol/rgb-consensus)
and remains under its original Apache-2.0 license.
