#!/usr/bin/env bash
#
# Run EVERY end-to-end demo against fresh nodes — the repo's real
# regression suite. `cargo test` covers the units; the demos are the
# evidence (anchors verified on-chain, covenants enforced by consensus,
# negatives rejected), so CI runs them all.
#
# Layout:
#   * one fresh stack (elementsd + elementsd-simplicity + bitcoind),
#     bootstrapped once;
#   * the Liquid/Bitcoin demos run in sequence on it;
#   * each Simplicity demo gets a FRESH simplicity node: they all seed
#     from the chain's single OP_TRUE genesis output, which only the
#     first run on a volume can claim.

set -euo pipefail

cd "$(dirname "$0")/.."

COMPOSE="${COMPOSE:-docker compose}"

banner() {
  echo
  echo "════════════════════════════════════════════════════════════"
  echo "  ▶ $1"
  echo "════════════════════════════════════════════════════════════"
}

reset_simplicity() {
  $COMPOSE rm -sf elementsd-simplicity > /dev/null 2>&1 || true
  local vols
  vols=$(docker volume ls -q | grep -- 'elementsd-simplicity-data$' || true)
  if [ -n "$vols" ]; then
    echo "$vols" | xargs docker volume rm > /dev/null 2>&1 || true
  fi
  $COMPOSE up -d elementsd-simplicity
  # Wait for RPC.
  for _ in $(seq 1 30); do
    if $COMPOSE exec -T elementsd-simplicity elements-cli -chain=elementsregtest \
        -rpcuser=user -rpcpassword=pass -rpcport=7042 getblockchaininfo > /dev/null 2>&1; then
      return
    fi
    sleep 1
  done
  echo "✗ simplicity node did not come up" >&2
  exit 1
}

banner "Fresh stack + bootstrap"
./scripts/teardown.sh || true
$COMPOSE up -d
./scripts/bootstrap.sh
./scripts/bootstrap_btc.sh
cargo build --workspace

# ── Liquid / Bitcoin demos (shared bootstrapped stack) ──────────────
for d in demo_seal demo_patched demo_rgb demo_rgb20 demo_confidential \
         demo_htlc demo_htlc_rgb demo_swap demo_backed_mint \
         demo_backed_schema demo_swap_roundtrip; do
  banner "$d"
  "./scripts/$d.sh"
done

# ── Simplicity demos (fresh node each) ──────────────────────────────
for d in demo_simplicity demo_mint_gate demo_mint_gate_burn demo_staking; do
  banner "$d (fresh Simplicity node)"
  reset_simplicity
  "./scripts/$d.sh"
done

banner "ALL DEMOS GREEN"
