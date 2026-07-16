#!/usr/bin/env bash
#
# Real RGB seal closing on Liquid.
#
# Unlike demo_rgb.sh, this demo:
#   1. Picks a SPECIFIC L-BTC UTXO from w_a and designates it the seal.
#   2. Builds a Liquid tx that SPENDS THAT EXACT UTXO and has the tapret
#      commitment as one of its outputs. We use createrawtransaction +
#      fundrawtransaction so we control the input.
#   3. Verifies BOTH that the seal was closed by this tx AND that the
#      tapret commitment is present.
#   4. Tampers in the obvious places to confirm rejection.

set -euo pipefail
source "$(dirname "$0")/_lib.sh"

cd "$(cd "$(dirname "$0")/.." && pwd)"

RGB_ANCHOR="${RGB_ANCHOR:-./target/debug/rgb-anchor}"
[ -x "$RGB_ANCHOR" ] || cargo build -p spike-rgb-anchor >&2

echo "════════════════════════════════════════════════════════════"
echo "  Liquid RGB seal closing (real seal, real witness tx)"
echo "════════════════════════════════════════════════════════════"

# 1. Pick a spendable L-BTC UTXO on w_a; we'll designate this the seal.
echo
echo "== Picking a seal UTXO from w_a =="
# Prefer an explicit v0-segwit L-BTC UTXO as the seal (raw signing of
# taproot/blinded inputs flakes on Elements); fall back to any UTXO.
SEAL=$(ecli_w w_a listunspent 1 | jq -r '([.[] | select(.asset=="b2e15d0d7a0c94e4e2ce0fe6e8691b9e451377f6e46e8045a86f7c4b5d4f0f23"
  and (.amountblinder == "0000000000000000000000000000000000000000000000000000000000000000")
  and (.address | startswith("ert1q")))] + .)[0]' )
SEAL_TXID=$(echo "$SEAL" | jq -r '.txid')
SEAL_VOUT=$(echo "$SEAL" | jq -r '.vout')
SEAL_AMOUNT=$(echo "$SEAL" | jq -r '.amount')
SEAL_SPK=$(echo "$SEAL" | jq -r '.scriptPubKey')
echo "  seal: $SEAL_TXID:$SEAL_VOUT  amount=$SEAL_AMOUNT  spk=${SEAL_SPK:0:16}..."

# 2. Build the anchor (carries the seal outpoint in its JSON).
echo
echo "== Build anchor (seal-aware) =="
OUT=$("$RGB_ANCHOR" build \
  --contracts usdt-liquid xaut-liquid \
  --chain-net liquid-testnet \
  --seal "$SEAL_TXID:$SEAL_VOUT")
ADDR=$(echo "$OUT" | sed -n '1p')
ANCHOR_JSON=$(echo "$OUT" | sed -n '2p')

ANCHOR_FILE="$OUT_DIR/anchor_m5.json"
echo "$ANCHOR_JSON" > "$ANCHOR_FILE"
echo "  saved -> $ANCHOR_FILE"
echo "  P2TR  -> $ADDR"

# 3. Construct a Liquid tx that SPENDS that seal UTXO and pays:
#    - 0.0005 L-BTC to the tapret address  (the commitment output)
#    - change back to w_a  (auto-added by fundrawtransaction)
#    - fee
LBTC="b2e15d0d7a0c94e4e2ce0fe6e8691b9e451377f6e46e8045a86f7c4b5d4f0f23"

echo
echo "== Building witness tx (must spend $SEAL_TXID:$SEAL_VOUT) =="

OUTPUTS_JSON=$(jq -n \
  --arg addr "$ADDR" \
  --arg asset "$LBTC" \
  '[{($addr): 0.0005, "asset": $asset}]')

RAW=$(ecli_w w_a createrawtransaction \
  "[{\"txid\":\"$SEAL_TXID\",\"vout\":$SEAL_VOUT}]" \
  "$OUTPUTS_JSON")

# fundrawtransaction adds change + fee. We pass `lockUnspents=true` to
# keep the seal pinned, and `add_inputs=true` to allow additional
# funding inputs if the seal alone isn't enough.
FUNDED=$(ecli_w w_a fundrawtransaction "$RAW" \
  '{"add_inputs": true, "lockUnspents": true}')
FUNDED_HEX=$(echo "$FUNDED" | jq -r '.hex')
echo "  funded tx prefix: ${FUNDED_HEX:0:80}..."

# Blind + sign + broadcast. The wallet will blind outputs going to its
# own confidential addresses (change) but the tapret output (which we
# derived ourselves as an unconfidential ert1p address) stays
# transparent.
BLINDED=$(ecli_w w_a blindrawtransaction "$FUNDED_HEX" 2>/dev/null || echo "$FUNDED_HEX")
SIGNED=$(ecli_w w_a signrawtransactionwithwallet "$BLINDED")
SIGNED_HEX=$(echo "$SIGNED" | jq -r '.hex')
COMPLETE=$(echo "$SIGNED" | jq -r '.complete')
echo "  signed: complete=$COMPLETE"

TXID=$(ecli sendrawtransaction "$SIGNED_HEX")
echo "  witness txid: $TXID"

ANY=$(ecli_w w_a getnewaddress)
ANY=$(ecli_w w_a getaddressinfo "$ANY" | jq -r '.unconfidential')
ecli generatetoaddress 2 "$ANY" > /dev/null

# Patch the txid into the anchor JSON.
jq --arg t "$TXID" '.txid = $t' "$ANCHOR_FILE" > "$ANCHOR_FILE.tmp" \
  && mv "$ANCHOR_FILE.tmp" "$ANCHOR_FILE"

# 4. Verify.
echo
echo "== Verify =="
"$RGB_ANCHOR" verify --anchor "$ANCHOR_FILE"

# 5. Negative tests.
echo
echo "== Negative: lie about the seal (replace seal.vout with 99) =="
BAD_SEAL="$OUT_DIR/anchor_m5_bad_seal.json"
jq '.seal.vout = 99' "$ANCHOR_FILE" > "$BAD_SEAL"
if "$RGB_ANCHOR" verify --anchor "$BAD_SEAL" 2>/dev/null; then
  echo "✗ FAIL: verifier accepted a wrong seal"
  exit 1
fi
echo "✓ verifier rejected wrong seal"

echo
echo "== Negative: tamper with one entry's message_hex =="
BAD_MSG="$OUT_DIR/anchor_m5_bad_msg.json"
jq '.entries[0].message_hex |= (.[0:62] + "ff")' "$ANCHOR_FILE" > "$BAD_MSG"
if "$RGB_ANCHOR" verify --anchor "$BAD_MSG" 2>/dev/null; then
  echo "✗ FAIL: verifier accepted tampered entry"
  exit 1
fi
echo "✓ verifier rejected tampered entry"

echo
echo "✅ Done — RGB seal closing verified on Liquid regtest."
echo "   The witness tx spends a real seal UTXO AND embeds the tapret"
echo "   commitment to a multi-entry MPC tree of two real RGB contracts."
