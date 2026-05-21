# Shared helpers for scripts/. Sourced, not executed.
#
# Usage:
#   source "$(dirname "$0")/_lib.sh"
#   ecli getblockchaininfo

set -euo pipefail

RPC_USER="${RPC_USER:-user}"
RPC_PASS="${RPC_PASS:-pass}"
CHAIN="${CHAIN:-elementsregtest}"
COMPOSE="${COMPOSE:-docker compose}"
OUT_DIR="${OUT_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/out}"

mkdir -p "$OUT_DIR"

# Run elements-cli inside the elementsd container.
ecli() {
  $COMPOSE exec -T elementsd \
    elements-cli \
    -chain="$CHAIN" \
    -rpcuser="$RPC_USER" \
    -rpcpassword="$RPC_PASS" \
    "$@"
}

# Run elements-cli scoped to a named wallet.
ecli_w() {
  local wallet="$1"; shift
  $COMPOSE exec -T elementsd \
    elements-cli \
    -chain="$CHAIN" \
    -rpcuser="$RPC_USER" \
    -rpcpassword="$RPC_PASS" \
    -rpcwallet="$wallet" \
    "$@"
}

# Wait until elementsd RPC responds.
wait_for_node() {
  local tries=60
  echo "Waiting for elementsd RPC..."
  while ! ecli getblockchaininfo > /dev/null 2>&1; do
    tries=$((tries - 1))
    if [ "$tries" -le 0 ]; then
      echo "elementsd did not become ready in time" >&2
      exit 1
    fi
    sleep 1
  done
  echo "elementsd ready."
}

# --- Bitcoin Core regtest -----------------------

# Run bitcoin-cli inside the bitcoind container.
bcli() {
  $COMPOSE exec -T bitcoind \
    bitcoin-cli -regtest -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASS" \
    "$@"
}

# Run bitcoin-cli scoped to a named wallet.
bcli_w() {
  local wallet="$1"; shift
  $COMPOSE exec -T bitcoind \
    bitcoin-cli -regtest -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASS" \
    -rpcwallet="$wallet" \
    "$@"
}

# Wait until bitcoind RPC responds.
wait_for_bitcoind() {
  local tries=60
  echo "Waiting for bitcoind RPC..."
  while ! bcli getblockchaininfo > /dev/null 2>&1; do
    tries=$((tries - 1))
    if [ "$tries" -le 0 ]; then
      echo "bitcoind did not become ready in time" >&2
      exit 1
    fi
    sleep 1
  done
  echo "bitcoind ready."
}
