#!/usr/bin/env bash
#
# Multi-entry RGB anchor on Liquid via the rgb-consensus 0.11
# production stack.
#
# Two real RGB contracts (each a Genesis stamped onto ChainNet::LiquidTestnet)
# share a single MPC tree. The tree's root is committed via TapretFirst
# in a Liquid P2TR output. The verifier:
#   1. rebuilds the MPC root from the entries (deterministic via static_entropy)
#   2. produces and checks an MPC inclusion proof per entry
#   3. recomputes the tapret tweak via rgb-consensus's dbc
#   4. fetches the on-chain Liquid tx and confirms scriptPubKey matches
#   5. runs rgb-consensus's unmodified ConvolveCommitProof::verify on the
#      Elements scriptPubKey

set -euo pipefail
source "$(dirname "$0")/_lib.sh"

cd "$(cd "$(dirname "$0")/.." && pwd)"

RGB_ANCHOR="${RGB_ANCHOR:-./target/debug/rgb-anchor}"
[ -x "$RGB_ANCHOR" ] || cargo build -p spike-rgb-anchor >&2

echo "════════════════════════════════════════════════════════════"
echo "  Multi-entry MPC, real RGB contracts, Liquid anchor"
echo "════════════════════════════════════════════════════════════"

ANCHOR_FILE="$OUT_DIR/anchor.json"

echo
echo "== Build =="
OUT=$("$RGB_ANCHOR" build \
  --contracts usdt-liquid xaut-liquid \
  --chain-net liquid-testnet)
ADDR=$(echo "$OUT" | sed -n '1p')
ANCHOR_JSON=$(echo "$OUT" | sed -n '2p')
echo "$ANCHOR_JSON" > "$ANCHOR_FILE"
echo "  saved -> $ANCHOR_FILE"

echo
echo "== Broadcast =="
TXID=$(ecli_w w_a sendtoaddress "$ADDR" 0.001)
echo "  txid -> $TXID"

ANY=$(ecli_w w_a getnewaddress)
ANY=$(ecli_w w_a getaddressinfo "$ANY" | jq -r '.unconfidential')
ecli generatetoaddress 2 "$ANY" > /dev/null

jq --arg t "$TXID" '.txid = $t' "$ANCHOR_FILE" > "$ANCHOR_FILE.tmp" \
  && mv "$ANCHOR_FILE.tmp" "$ANCHOR_FILE"

echo
echo "== Verify =="
"$RGB_ANCHOR" verify --anchor "$ANCHOR_FILE"

echo
echo "== Negative: tamper with one entry's message_hex =="
BAD="$OUT_DIR/anchor_bad.json"
jq '.entries[0].message_hex |= (.[0:62] + "ff")' "$ANCHOR_FILE" > "$BAD"
if "$RGB_ANCHOR" verify --anchor "$BAD" 2>/dev/null; then
  echo "✗ FAIL: verifier accepted tampered entry"
  exit 1
fi
echo "✓ rejected tampered entry"

echo
echo "✅ Done — two real RGB contracts share one Liquid anchor."
echo "   Inclusion proofs verify per-contract. rgb-consensus's unmodified"
echo "   verifier accepts the Liquid scriptPubKey."
