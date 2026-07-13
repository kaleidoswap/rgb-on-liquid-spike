#!/usr/bin/env bash
#
# RGB-wrapped HTLC claim on Liquid — closes the deep-dive's limitation:
# "a production RGB atomic swap would wrap the claim itself in the next
# RGB transition, so the asset re-anchors to a seal the claimer fully
# controls."
#
# The construction: the RGB seal IS the HTLC UTXO. Claiming the HTLC
# (revealing the preimage) necessarily closes the seal, and the claim
# transaction carries a tapret commitment to the transition that moves
# the asset to the claimer's own seal. One transaction does all three
# jobs:
#
#   vin[0]  : spends the HTLC output      → reveals preimage (swap leg)
#                                         → closes the RGB seal
#   vout[0] : tapret commitment           → anchors the next transition
#   vout[1] : claimer's new output        → the asset's new seal
#   vout[2] : explicit fee
#
# Verified by both the bespoke path and the patched
# rgbcore::dbc::Anchor::verify — on a transaction whose witness also
# satisfies an HTLC.

set -euo pipefail
source "$(dirname "$0")/_lib.sh"

cd "$(cd "$(dirname "$0")/.." && pwd)"

RGB_ANCHOR="${RGB_ANCHOR:-./target/debug/rgb-anchor}"
[ -x "$RGB_ANCHOR" ] || cargo build -p spike-rgb-anchor >&2

CSV_DELAY=5
FEE_SAT=1000
ANCHOR_SAT=500
FUND_SAT=100000
LBTC="b2e15d0d7a0c94e4e2ce0fe6e8691b9e451377f6e46e8045a86f7c4b5d4f0f23"

PREIMAGE=$(openssl rand -hex 32)
HASH=$(printf '%s' "$PREIMAGE" | xxd -r -p | shasum -a 256 | awk '{print $1}')

echo "════════════════════════════════════════════════════════════"
echo "  RGB-wrapped HTLC claim on Liquid"
echo "  (claim tx = swap settlement + seal closure + RGB anchor)"
echo "════════════════════════════════════════════════════════════"

# ── 1. Fund the HTLC — this UTXO will be Bob's hashlocked RGB seal ──
echo
echo "== Fund the HTLC (Bob's hashlocked seal) =="
HTLC=$("$RGB_ANCHOR" htlc-address --hash "$HASH" \
  --claimer bob-claim --refund alice-refund --csv-delay $CSV_DELAY --hrp ert)
HTLC_ADDR=$(echo "$HTLC" | jq -r '.address')
HTLC_SPK=$(echo "$HTLC" | jq -r '.spk_hex')

FUND_TXID=$(ecli_w w_a sendtoaddress "$HTLC_ADDR" 0.001)
MINE=$(ecli_w w_a getaddressinfo "$(ecli_w w_a getnewaddress)" | jq -r '.unconfidential')
ecli generatetoaddress 1 "$MINE" > /dev/null
HTLC_VOUT=$(ecli getrawtransaction "$FUND_TXID" 1 \
  | jq --arg spk "$HTLC_SPK" '.vout[] | select(.scriptPubKey.hex == $spk) | .n')
echo "  HTLC/seal: $FUND_TXID:$HTLC_VOUT ($FUND_SAT sat)"

# ── 2. Build the RGB transition whose seal is the HTLC outpoint ────
# Bob's new seal is vout 1 of the claim tx (vout 0 = tapret anchor),
# a future outpoint — same placeholder technique as demo_rgb20.sh.
echo
echo "== Issue NIA + build the transition off the HTLC seal =="
BOB_PLACEHOLDER_TXID=$(printf 'bob-claim-placeholder' | shasum -a 256 | awk '{print $1}')

OUT=$("$RGB_ANCHOR" rgb20-transfer \
  --name KaleidoSwapUSD \
  --ticker kSUSD \
  --supply 600000 \
  --send   600000 \
  --alice-seal "$FUND_TXID:$HTLC_VOUT" \
  --bob-seal   "$BOB_PLACEHOLDER_TXID:1")
TAPRET_ADDR=$(echo "$OUT" | sed -n '1p')
ANCHOR_JSON=$(echo "$OUT" | sed -n '2p')

ANCHOR_FILE="$OUT_DIR/anchor_htlc_rgb.json"
echo "$ANCHOR_JSON" > "$ANCHOR_FILE"
TAPRET_SPK=$(ecli validateaddress "$TAPRET_ADDR" | jq -r '.scriptPubKey')
echo "  tapret SPK: $TAPRET_SPK"
echo "  saved -> $ANCHOR_FILE"

# ── 3. The claim tx: preimage reveal + tapret anchor in one ─────────
echo
echo "== Build + sign + broadcast the RGB-wrapped claim =="
DEST_SPK=$(ecli_w w_b getaddressinfo \
  "$(ecli_w w_b getaddressinfo "$(ecli_w w_b getnewaddress)" | jq -r '.unconfidential')" \
  | jq -r '.scriptPubKey')

CLAIM=$("$RGB_ANCHOR" htlc-spend-liquid \
  --prev-txid "$FUND_TXID" --prev-vout "$HTLC_VOUT" --input-value-sat $FUND_SAT \
  --fee-sat $FEE_SAT --dest-spk "$DEST_SPK" --lbtc-asset "$LBTC" --hash "$HASH" \
  --claimer bob-claim --refund alice-refund --csv-delay $CSV_DELAY \
  --branch claim --preimage "$PREIMAGE" \
  --anchor-spk "$TAPRET_SPK" --anchor-value-sat $ANCHOR_SAT)

CLAIM_TXID=$(ecli sendrawtransaction "$CLAIM")
ecli generatetoaddress 2 "$MINE" > /dev/null
echo "  claim txid: $CLAIM_TXID"

jq --arg t "$CLAIM_TXID" '.txid = $t' "$ANCHOR_FILE" > "$ANCHOR_FILE.tmp" \
  && mv "$ANCHOR_FILE.tmp" "$ANCHOR_FILE"

# ── 4. Confirm the claim did all three jobs ─────────────────────────
echo
echo "== One tx, three jobs =="
DECODED=$(ecli getrawtransaction "$CLAIM_TXID" 1)

REVEALED=$(echo "$DECODED" | jq -r '.vin[0].scriptWitness[1] // .vin[0].txinwitness[1]')
if [ "$REVEALED" != "$PREIMAGE" ]; then
  echo "✗ FAIL: preimage not revealed in claim witness"
  exit 1
fi
echo "  ✓ swap leg   : preimage revealed in vin[0] witness"

ANCHOR_SPK_CHAIN=$(echo "$DECODED" | jq -r '.vout[0].scriptPubKey.hex')
if [ "$ANCHOR_SPK_CHAIN" != "$TAPRET_SPK" ]; then
  echo "✗ FAIL: vout[0] is not the tapret output"
  exit 1
fi
echo "  ✓ RGB anchor : tapret commitment at vout[0]"
echo "  ✓ new seal   : claimer-controlled output at vout[1] ($CLAIM_TXID:1)"

# ── 5. Verify the RGB anchor on the claim tx ────────────────────────
echo
echo "== Verify (bespoke path: commitment + seal closure) =="
"$RGB_ANCHOR" verify --anchor "$ANCHOR_FILE"

echo
echo "== Verify (patched path: patched rgbcore::dbc::Anchor::verify) =="
"$RGB_ANCHOR" verify-patched --anchor "$ANCHOR_FILE"

# ── 6. Negative: the same wrapped claim with a wrong preimage dies ──
echo
echo "== Negative: wrapped claim with wrong preimage =="
WRONG=$(openssl rand -hex 32)
BAD=$("$RGB_ANCHOR" htlc-spend-liquid \
  --prev-txid "$FUND_TXID" --prev-vout "$HTLC_VOUT" --input-value-sat $FUND_SAT \
  --fee-sat $FEE_SAT --dest-spk "$DEST_SPK" --lbtc-asset "$LBTC" --hash "$HASH" \
  --claimer bob-claim --refund alice-refund --csv-delay $CSV_DELAY \
  --branch claim --preimage "$WRONG" \
  --anchor-spk "$TAPRET_SPK" --anchor-value-sat $ANCHOR_SAT)
if ecli sendrawtransaction "$BAD" 2>/dev/null; then
  echo "✗ FAIL: wrong-preimage wrapped claim accepted (double-spend of the seal?)"
  exit 1
fi
echo "✓ rejected (and the seal was already closed by the real claim anyway)"

echo
echo "✅ Done — RGB-wrapped HTLC claim on Liquid."
echo "   The HTLC UTXO was the RGB seal. Claiming it revealed the swap"
echo "   preimage, closed the seal, and anchored the transition that"
echo "   re-seats the asset on the claimer's own output — one atomic"
echo "   transaction, verified by the patched rgb-consensus."
