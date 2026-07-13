#!/usr/bin/env bash
#
# Full HTLC on both chains — the production-shaped successor to the
# minimal hashlock used by demo_swap.sh.
#
#   OP_IF    OP_SHA256 <H> OP_EQUALVERIFY <claimerPk> OP_CHECKSIG
#   OP_ELSE  <T> OP_CSV OP_DROP <refundPk> OP_CHECKSIG
#   OP_ENDIF
#
# On EACH chain (Bitcoin regtest, Liquid regtest) this script proves:
#   1. claim with the right preimage + claimer key  → accepted
#   2. claim with a wrong preimage                  → rejected by consensus
#   3. refund before the CSV timeout                → rejected (non-BIP68-final)
#   4. refund after the CSV timeout + refund key    → accepted
#
# The same witness-script BYTES are used on both chains — only the
# address HRP and the tx envelope differ.

set -euo pipefail
source "$(dirname "$0")/_lib.sh"

cd "$(cd "$(dirname "$0")/.." && pwd)"

RGB_ANCHOR="${RGB_ANCHOR:-./target/debug/rgb-anchor}"
[ -x "$RGB_ANCHOR" ] || cargo build -p spike-rgb-anchor >&2

CSV_DELAY=5
FEE_SAT=1000
FUND_AMT=0.001          # 100_000 sat
FUND_SAT=100000
LBTC="b2e15d0d7a0c94e4e2ce0fe6e8691b9e451377f6e46e8045a86f7c4b5d4f0f23"

PREIMAGE=$(openssl rand -hex 32)
HASH=$(printf '%s' "$PREIMAGE" | xxd -r -p | shasum -a 256 | awk '{print $1}')
WRONG_PREIMAGE=$(openssl rand -hex 32)

echo "════════════════════════════════════════════════════════════"
echo "  Full HTLC (claim + CSV refund) on Bitcoin AND Liquid"
echo "════════════════════════════════════════════════════════════"
echo "  preimage : $PREIMAGE"
echo "  H        : $HASH"
echo "  CSV delay: $CSV_DELAY blocks"

# ─────────────────────────────────────────────────────────────────────
# Helpers
# ─────────────────────────────────────────────────────────────────────

# find_vout <decoded_tx_json> <spk_hex> → vout index
find_vout() {
  echo "$1" | jq --arg spk "$2" '.vout[] | select(.scriptPubKey.hex == $spk) | .n'
}

# expect_reject <chain_cli_send_result_capture>: run "$@", expect failure
expect_reject() {
  local what="$1"; shift
  if OUT=$("$@" 2>&1); then
    echo "✗ FAIL: $what was ACCEPTED (should be rejected): $OUT"
    exit 1
  fi
  echo "  ✓ rejected as expected ($what)"
  echo "    └─ $(echo "$OUT" | head -1)"
}

# ═════════════════════════════════════════════════════════════════════
#  BITCOIN LEG
# ═════════════════════════════════════════════════════════════════════
echo
echo "════════ BITCOIN — HTLC #1 (claim path) ════════"

HTLC_BTC=$("$RGB_ANCHOR" htlc-address --hash "$HASH" \
  --claimer alice-claim --refund bob-refund --csv-delay $CSV_DELAY --hrp bcrt)
ADDR_BTC=$(echo "$HTLC_BTC" | jq -r '.address')
SPK_BTC=$(echo "$HTLC_BTC" | jq -r '.spk_hex')
echo "  HTLC addr: $ADDR_BTC"

FUND_TXID=$(bcli_w w_btc sendtoaddress "$ADDR_BTC" $FUND_AMT)
MINE_BTC=$(bcli_w w_btc getnewaddress)
bcli generatetoaddress 1 "$MINE_BTC" > /dev/null
DECODED=$(bcli getrawtransaction "$FUND_TXID" 1)
VOUT=$(find_vout "$DECODED" "$SPK_BTC")
echo "  funded   : $FUND_TXID:$VOUT ($FUND_SAT sat)"

DEST_BTC=$(bcli_w w_btc getaddressinfo "$(bcli_w w_btc getnewaddress)" | jq -r '.scriptPubKey')

echo
echo "-- Negative: claim with WRONG preimage --"
BAD_CLAIM=$("$RGB_ANCHOR" htlc-spend-btc \
  --prev-txid "$FUND_TXID" --prev-vout "$VOUT" --input-value-sat $FUND_SAT \
  --fee-sat $FEE_SAT --dest-spk "$DEST_BTC" --hash "$HASH" \
  --claimer alice-claim --refund bob-refund --csv-delay $CSV_DELAY \
  --branch claim --preimage "$WRONG_PREIMAGE")
expect_reject "wrong-preimage claim" bcli sendrawtransaction "$BAD_CLAIM"

echo
echo "-- Negative: refund BEFORE the CSV timeout --"
EARLY_REFUND=$("$RGB_ANCHOR" htlc-spend-btc \
  --prev-txid "$FUND_TXID" --prev-vout "$VOUT" --input-value-sat $FUND_SAT \
  --fee-sat $FEE_SAT --dest-spk "$DEST_BTC" --hash "$HASH" \
  --claimer alice-claim --refund bob-refund --csv-delay $CSV_DELAY \
  --branch refund)
expect_reject "early refund" bcli sendrawtransaction "$EARLY_REFUND"

echo
echo "-- Positive: claim with the RIGHT preimage --"
GOOD_CLAIM=$("$RGB_ANCHOR" htlc-spend-btc \
  --prev-txid "$FUND_TXID" --prev-vout "$VOUT" --input-value-sat $FUND_SAT \
  --fee-sat $FEE_SAT --dest-spk "$DEST_BTC" --hash "$HASH" \
  --claimer alice-claim --refund bob-refund --csv-delay $CSV_DELAY \
  --branch claim --preimage "$PREIMAGE")
CLAIM_TXID=$(bcli sendrawtransaction "$GOOD_CLAIM")
bcli generatetoaddress 1 "$MINE_BTC" > /dev/null
echo "  ✓ claim accepted: $CLAIM_TXID"

REVEALED=$(bcli getrawtransaction "$CLAIM_TXID" 1 | jq -r '.vin[0].txinwitness[1]')
if [ "$REVEALED" != "$PREIMAGE" ]; then
  echo "✗ FAIL: preimage not revealed in claim witness"
  exit 1
fi
echo "  ✓ preimage revealed on-chain in the claim witness"

echo
echo "════════ BITCOIN — HTLC #2 (refund path) ════════"
FUND2_TXID=$(bcli_w w_btc sendtoaddress "$ADDR_BTC" $FUND_AMT)
bcli generatetoaddress 1 "$MINE_BTC" > /dev/null
DECODED2=$(bcli getrawtransaction "$FUND2_TXID" 1)
VOUT2=$(find_vout "$DECODED2" "$SPK_BTC")
echo "  funded   : $FUND2_TXID:$VOUT2"

echo "  mining $CSV_DELAY blocks to mature the CSV timeout…"
bcli generatetoaddress $CSV_DELAY "$MINE_BTC" > /dev/null

REFUND=$("$RGB_ANCHOR" htlc-spend-btc \
  --prev-txid "$FUND2_TXID" --prev-vout "$VOUT2" --input-value-sat $FUND_SAT \
  --fee-sat $FEE_SAT --dest-spk "$DEST_BTC" --hash "$HASH" \
  --claimer alice-claim --refund bob-refund --csv-delay $CSV_DELAY \
  --branch refund)
REFUND_TXID=$(bcli sendrawtransaction "$REFUND")
bcli generatetoaddress 1 "$MINE_BTC" > /dev/null
echo "  ✓ refund accepted after timeout: $REFUND_TXID"

# ═════════════════════════════════════════════════════════════════════
#  LIQUID LEG — same witness-script bytes, Elements envelope
# ═════════════════════════════════════════════════════════════════════
echo
echo "════════ LIQUID — HTLC #1 (claim path) ════════"

HTLC_LQ=$("$RGB_ANCHOR" htlc-address --hash "$HASH" \
  --claimer alice-claim --refund bob-refund --csv-delay $CSV_DELAY --hrp ert)
ADDR_LQ=$(echo "$HTLC_LQ" | jq -r '.address')
SPK_LQ=$(echo "$HTLC_LQ" | jq -r '.spk_hex')
if [ "$SPK_LQ" != "$SPK_BTC" ]; then
  echo "✗ FAIL: SPK differs across chains (should be identical bytes)"
  exit 1
fi
echo "  HTLC addr: $ADDR_LQ (same witness program as Bitcoin ✓)"

LFUND_TXID=$(ecli_w w_a sendtoaddress "$ADDR_LQ" $FUND_AMT)
MINE_LQ=$(ecli_w w_a getaddressinfo "$(ecli_w w_a getnewaddress)" | jq -r '.unconfidential')
ecli generatetoaddress 1 "$MINE_LQ" > /dev/null
LDECODED=$(ecli getrawtransaction "$LFUND_TXID" 1)
LVOUT=$(find_vout "$LDECODED" "$SPK_LQ")
echo "  funded   : $LFUND_TXID:$LVOUT ($FUND_SAT sat L-BTC, explicit)"

DEST_LQ=$(ecli_w w_b getaddressinfo "$(ecli_w w_b getaddressinfo "$(ecli_w w_b getnewaddress)" | jq -r '.unconfidential')" | jq -r '.scriptPubKey')

echo
echo "-- Negative: claim with WRONG preimage --"
LBAD_CLAIM=$("$RGB_ANCHOR" htlc-spend-liquid \
  --prev-txid "$LFUND_TXID" --prev-vout "$LVOUT" --input-value-sat $FUND_SAT \
  --fee-sat $FEE_SAT --dest-spk "$DEST_LQ" --lbtc-asset "$LBTC" --hash "$HASH" \
  --claimer alice-claim --refund bob-refund --csv-delay $CSV_DELAY \
  --branch claim --preimage "$WRONG_PREIMAGE")
expect_reject "wrong-preimage claim (Liquid)" ecli sendrawtransaction "$LBAD_CLAIM"

echo
echo "-- Negative: refund BEFORE the CSV timeout --"
LEARLY_REFUND=$("$RGB_ANCHOR" htlc-spend-liquid \
  --prev-txid "$LFUND_TXID" --prev-vout "$LVOUT" --input-value-sat $FUND_SAT \
  --fee-sat $FEE_SAT --dest-spk "$DEST_LQ" --lbtc-asset "$LBTC" --hash "$HASH" \
  --claimer alice-claim --refund bob-refund --csv-delay $CSV_DELAY \
  --branch refund)
expect_reject "early refund (Liquid)" ecli sendrawtransaction "$LEARLY_REFUND"

echo
echo "-- Positive: claim with the RIGHT preimage --"
LGOOD_CLAIM=$("$RGB_ANCHOR" htlc-spend-liquid \
  --prev-txid "$LFUND_TXID" --prev-vout "$LVOUT" --input-value-sat $FUND_SAT \
  --fee-sat $FEE_SAT --dest-spk "$DEST_LQ" --lbtc-asset "$LBTC" --hash "$HASH" \
  --claimer alice-claim --refund bob-refund --csv-delay $CSV_DELAY \
  --branch claim --preimage "$PREIMAGE")
LCLAIM_TXID=$(ecli sendrawtransaction "$LGOOD_CLAIM")
ecli generatetoaddress 1 "$MINE_LQ" > /dev/null
echo "  ✓ claim accepted: $LCLAIM_TXID"

LREVEALED=$(ecli getrawtransaction "$LCLAIM_TXID" 1 | jq -r '.vin[0].scriptWitness[1] // .vin[0].txinwitness[1]')
if [ "$LREVEALED" != "$PREIMAGE" ]; then
  echo "✗ FAIL: preimage not revealed in Liquid claim witness (got: $LREVEALED)"
  exit 1
fi
echo "  ✓ preimage revealed on-chain in the claim witness"

echo
echo "════════ LIQUID — HTLC #2 (refund path) ════════"
LFUND2_TXID=$(ecli_w w_a sendtoaddress "$ADDR_LQ" $FUND_AMT)
ecli generatetoaddress 1 "$MINE_LQ" > /dev/null
LDECODED2=$(ecli getrawtransaction "$LFUND2_TXID" 1)
LVOUT2=$(find_vout "$LDECODED2" "$SPK_LQ")
echo "  funded   : $LFUND2_TXID:$LVOUT2"

echo "  mining $CSV_DELAY blocks to mature the CSV timeout…"
ecli generatetoaddress $CSV_DELAY "$MINE_LQ" > /dev/null

LREFUND=$("$RGB_ANCHOR" htlc-spend-liquid \
  --prev-txid "$LFUND2_TXID" --prev-vout "$LVOUT2" --input-value-sat $FUND_SAT \
  --fee-sat $FEE_SAT --dest-spk "$DEST_LQ" --lbtc-asset "$LBTC" --hash "$HASH" \
  --claimer alice-claim --refund bob-refund --csv-delay $CSV_DELAY \
  --branch refund)
LREFUND_TXID=$(ecli sendrawtransaction "$LREFUND")
ecli generatetoaddress 1 "$MINE_LQ" > /dev/null
echo "  ✓ refund accepted after timeout: $LREFUND_TXID"

echo
echo "✅ Done — full HTLC exercised on Bitcoin AND Liquid:"
echo "   claim(right preimage) ✓   claim(wrong preimage) ✗ rejected"
echo "   refund(early) ✗ rejected  refund(after CSV) ✓"
echo "   Same witness-script bytes on both chains."
