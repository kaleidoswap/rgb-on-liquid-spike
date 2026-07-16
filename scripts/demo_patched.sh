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
# Pick the input ourselves: an explicit (unblinded) v0-segwit L-BTC
# UTXO. Letting fundrawtransaction roam can pull in a taproot or
# blinded input, and Elements' raw signing then produces an invalid
# Schnorr signature (same flake demo_swap guards against).
pick_in() {
  ecli_w w_a listunspent 1 | jq -r '[.[] | select(.asset=="'"$LBTC"'"
    and (.amountblinder == "0000000000000000000000000000000000000000000000000000000000000000")
    and (.address | startswith("ert1q"))
    and (.amount >= 0.002))][0]'
}
IN_UTXO=$(pick_in)
if [ "$IN_UTXO" = "null" ]; then
  # Wallet funds are blinded change by now; surface an explicit UTXO.
  EXP=$(ecli_w w_a getaddressinfo "$(ecli_w w_a getnewaddress)" | jq -r '.unconfidential')
  ecli_w w_a sendtoaddress "$EXP" 0.01 > /dev/null
  MINE_TMP=$(ecli_w w_a getaddressinfo "$(ecli_w w_a getnewaddress)" | jq -r '.unconfidential')
  ecli generatetoaddress 1 "$MINE_TMP" > /dev/null
  IN_UTXO=$(pick_in)
fi
[ "$IN_UTXO" != "null" ] || { echo "✗ no explicit v0 L-BTC UTXO in w_a" >&2; exit 1; }
IN_TXID=$(echo "$IN_UTXO" | jq -r '.txid'); IN_VOUT=$(echo "$IN_UTXO" | jq -r '.vout')
IN_AMT=$(echo "$IN_UTXO" | jq -r '.amount')
CHANGE_ADDR=$(ecli_w w_a getaddressinfo "$(ecli_w w_a getnewaddress)" | jq -r '.unconfidential')
CHANGE=$(python3 -c "from decimal import Decimal as D; print((D('$IN_AMT')-D('0.001')-D('0.00005')).quantize(D('0.00000001')))")
OUTPUTS_JSON=$(jq -n --arg addr "$ADDR" --arg ch "$CHANGE_ADDR" --arg asset "$LBTC" \
  --arg change "$CHANGE" \
  '[{($addr): 0.001, "asset": $asset},
    {($ch): ($change|tonumber), "asset": $asset},
    {"fee": 0.00005, "asset": $asset}]')
RAW=$(ecli_w w_a createrawtransaction \
  "[{\"txid\":\"$IN_TXID\",\"vout\":$IN_VOUT}]" "$OUTPUTS_JSON")
SIGNED=$(ecli_w w_a signrawtransactionwithwallet "$RAW")
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
