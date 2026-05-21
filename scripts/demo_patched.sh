#!/usr/bin/env bash
#
# Verify an RGB anchor on Liquid through the patched rgbcore::dbc::Anchor.
#
# This demo calls `rgbcore::dbc::Anchor::verify(pid, msg, &witness_tx)`
# on a Liquid transaction, with `witness_tx` being our Elements adapter
# implementing the new `WitnessTx` trait.
#
# If the upstream rgb-protocol/rgb-consensus PR for `WitnessTx` lands
# unchanged, this is literally the integration code an RGB-on-Liquid
# wallet would ship: one `impl WitnessTx for your_liquid_tx` and you
# can call `Anchor::verify` on Liquid.

set -euo pipefail
source "$(dirname "$0")/_lib.sh"

cd "$(cd "$(dirname "$0")/.." && pwd)"

RGB_ANCHOR="${RGB_ANCHOR:-./target/debug/rgb-anchor}"
[ -x "$RGB_ANCHOR" ] || cargo build -p spike-rgb-anchor >&2

echo "════════════════════════════════════════════════════════════"
echo "  PATCHED rgbcore::dbc::Anchor::verify on Liquid"
echo "════════════════════════════════════════════════════════════"

echo
echo "== Build single-entry anchor (so patched verify rebuilds it cleanly) =="
OUT=$("$RGB_ANCHOR" build \
  --contracts usdt-liquid \
  --chain-net liquid-testnet)
ADDR=$(echo "$OUT" | sed -n '1p')
ANCHOR_JSON=$(echo "$OUT" | sed -n '2p')

ANCHOR_FILE="$OUT_DIR/anchor_m6.json"
echo "$ANCHOR_JSON" > "$ANCHOR_FILE"

echo
echo "== Broadcast (commitment MUST be first P2TR output per TapretFirst convention) =="
# Build tx with our tapret address at vout[0], let fundrawtransaction
# add change at a specific later position.
LBTC="b2e15d0d7a0c94e4e2ce0fe6e8691b9e451377f6e46e8045a86f7c4b5d4f0f23"
OUTPUTS_JSON=$(jq -n --arg addr "$ADDR" --arg asset "$LBTC" \
  '[{($addr): 0.001, "asset": $asset}]')
RAW=$(ecli_w w_a createrawtransaction '[]' "$OUTPUTS_JSON")
# changePosition=1 ensures change goes after our tapret output.
FUNDED=$(ecli_w w_a fundrawtransaction "$RAW" '{"changePosition": 1, "add_inputs": true}')
FUNDED_HEX=$(echo "$FUNDED" | jq -r '.hex')
BLINDED=$(ecli_w w_a blindrawtransaction "$FUNDED_HEX" 2>/dev/null || echo "$FUNDED_HEX")
SIGNED=$(ecli_w w_a signrawtransactionwithwallet "$BLINDED")
SIGNED_HEX=$(echo "$SIGNED" | jq -r '.hex')
TXID=$(ecli sendrawtransaction "$SIGNED_HEX")
echo "  txid -> $TXID"

ANY=$(ecli_w w_a getnewaddress)
ANY=$(ecli_w w_a getaddressinfo "$ANY" | jq -r '.unconfidential')
ecli generatetoaddress 2 "$ANY" > /dev/null

jq --arg t "$TXID" '.txid = $t' "$ANCHOR_FILE" > "$ANCHOR_FILE.tmp" \
  && mv "$ANCHOR_FILE.tmp" "$ANCHOR_FILE"

echo
echo "== verify-patched: call PATCHED rgbcore::dbc::Anchor::verify =="
"$RGB_ANCHOR" verify-patched --anchor "$ANCHOR_FILE"

echo
echo "✅ Done — the WitnessTx patch verified a Liquid witness tx."
echo "   With the upstream PR merged, an Elements/Liquid wallet ships"
echo "   the same integration: impl WitnessTx for el::Transaction."
