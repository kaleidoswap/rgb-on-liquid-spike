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
- A cross-chain atomic swap: a real RGB20 asset on Bitcoin and one on Liquid,
  exchanged through a shared hashlock, with no custodian.
- The patch itself: `rgb-consensus` 0.11.1-rc.10 plus a `WitnessTx` trait,
  vendored under `vendor/rgb-consensus-patched/`. The upstream test suite
  passes unchanged (45/45), and `rgb-ops`, `rgb-schemas`, `rgb-invoicing`, and
  `rgb-aluvm` all compile against the patched crate with no source changes.

## Layout

```
.
├── RFC.md                      the proposed upstream change
├── docker-compose.yml          elementsd + bitcoind regtest
├── docker/elements.conf        Liquid regtest config
├── scripts/                    regtest bootstrap, end-to-end demos, teardown
├── crates/
│   ├── spike-env/              minimal JSON-RPC client for elementsd / bitcoind
│   ├── spike-tapret/           BIP-341 tweak math + P2TR address encoding
│   └── spike-rgb-anchor/       MPC tree, tapret commit/verify, RGB20, atomic swap
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
./scripts/demo_swap.sh          # cross-chain Bitcoin <-> Liquid atomic swap

./scripts/teardown.sh           # stop the nodes and wipe state
```

Two narrower demos are also available: `./scripts/demo_rgb.sh` (a multi-entry
MPC anchor) and `./scripts/demo_seal.sh` (seal-closure verification, with
negative tests).

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

## Next steps

This is a proof of concept on regtest, not a product. Shipping it takes:

- **Upstreaming the patch** to `rgb-protocol/rgb-consensus`.
- **A Liquid resolver** so a wallet can fetch Liquid witness transactions
  during consignment validation — an adapter over LWK or an Elements-aware
  Esplora.
- **A Liquid backend for `rgb-lib`**, exposing the same issue / send / receive
  API the Bitcoin path already provides.
- **A swap coordinator** that hardens the cross-chain hashlock into a full
  Hash Time-Locked Contract with timeouts and a refund branch.

None of these are research problems.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your
option.

`vendor/rgb-consensus-patched/` is a derivative of
[`rgb-protocol/rgb-consensus`](https://github.com/rgb-protocol/rgb-consensus)
and remains under its original Apache-2.0 license.
