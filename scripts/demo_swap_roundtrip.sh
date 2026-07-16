#!/usr/bin/env bash
#
# Round-trip atomic swap: RGB assets crossing Bitcoin <-> Liquid and
# coming back, with atomic swaps as the router.
#
# RGB assets never leave their issuance chain: oUSD lives on Bitcoin,
# lUSD lives on Liquid, and both supplies are conserved throughout.
# What crosses chains is OWNERSHIP, via shared-preimage HTLCs whose
# claims are RGB-wrapped: the claim transaction reveals the preimage
# (settling the swap leg), closes the RGB seal, and anchors the
# transition that re-seats the asset — one atomic transaction per leg.
#
#   Swap 1 (preimage P1):  Alice's oUSD (BTC)  <->  Bob's lUSD (Liquid)
#   Swap 2 (preimage P2):  the same swap, reversed — everything returns
#
# Swap 2 is the part swap 1 demos usually skip: it needs CHAINED
# transfers (the return lock consumes the allocation the first claim
# created), which is what `rgb20-transfer --consume-opid` adds.
#
# Every hop is verified through the patched rgb-consensus
# (`rgbcore::dbc::Anchor::verify` over the WitnessTx trait) — the same
# code path on the Bitcoin and the Liquid transactions.

set -euo pipefail
source "$(dirname "$0")/_lib.sh"

cd "$(cd "$(dirname "$0")/.." && pwd)"

RGB_ANCHOR="${RGB_ANCHOR:-./target/debug/rgb-anchor}"
[ -x "$RGB_ANCHOR" ] || cargo build -p spike-rgb-anchor >&2

SUPPLY=100000
CSV_DELAY=5
LBTC="b2e15d0d7a0c94e4e2ce0fe6e8691b9e451377f6e46e8045a86f7c4b5d4f0f23"
FUND_SAT=100000            # sats carried by each HTLC / seal UTXO
FEE_SAT=500
ANCHOR_SAT=500

echo "════════════════════════════════════════════════════════════"
echo "  Round-trip atomic swap: Bitcoin RGB20 <-> Liquid RGB20"
echo "════════════════════════════════════════════════════════════"

# ── plumbing ────────────────────────────────────────────────────────
MINE_L=$(ecli_w w_a getaddressinfo "$(ecli_w w_a getnewaddress)" | jq -r '.unconfidential')
MINE_B=$(bcli_w w_btc getnewaddress)

fund_btc() {  # <address|-> prints "txid:vout spk" for a fresh 0.001 BTC UTXO
  local addr spk txid vout
  addr=$(bcli_w w_btc getnewaddress)
  spk=$(bcli getaddressinfo "$addr" 2>/dev/null | jq -r '.scriptPubKey') \
    || spk=$(bcli_w w_btc getaddressinfo "$addr" | jq -r '.scriptPubKey')
  txid=$(bcli_w w_btc sendtoaddress "$addr" 0.001)
  bcli_w w_btc generatetoaddress 1 "$MINE_B" > /dev/null
  vout=$(bcli getrawtransaction "$txid" 1 | jq --arg s "$spk" '.vout[] | select(.scriptPubKey.hex == $s) | .n')
  echo "$txid:$vout $spk"
}

fund_liq() {  # <wallet> prints "txid:vout spk"
  local w="$1" addr spk txid vout
  addr=$(ecli_w "$w" getaddressinfo "$(ecli_w "$w" getnewaddress)" | jq -r '.unconfidential')
  spk=$(ecli_w "$w" getaddressinfo "$addr" | jq -r '.scriptPubKey')
  txid=$(ecli_w "$w" sendtoaddress "$addr" 0.001)
  ecli generatetoaddress 1 "$MINE_L" > /dev/null
  vout=$(ecli getrawtransaction "$txid" 1 | jq --arg s "$spk" '.vout[] | select(.scriptPubKey.hex == $s) | .n')
  echo "$txid:$vout $spk"
}

fund_htlc_btc() {  # <addr> <spk> prints "txid:vout"
  local txid vout
  txid=$(bcli_w w_btc sendtoaddress "$1" 0.001)
  bcli_w w_btc generatetoaddress 1 "$MINE_B" > /dev/null
  vout=$(bcli getrawtransaction "$txid" 1 | jq --arg s "$2" '.vout[] | select(.scriptPubKey.hex == $s) | .n')
  echo "$txid:$vout"
}

fund_htlc_liq() {  # <addr> <spk> prints "txid:vout"
  local txid vout
  txid=$(ecli_w w_a sendtoaddress "$1" 0.001)
  ecli generatetoaddress 1 "$MINE_L" > /dev/null
  vout=$(ecli getrawtransaction "$txid" 1 | jq --arg s "$2" '.vout[] | select(.scriptPubKey.hex == $s) | .n')
  echo "$txid:$vout"
}

assert_closes() {  # <chain btc|liq> <txid> <seal txid:vout> <label>
  local dec
  if [ "$1" = btc ]; then dec=$(bcli getrawtransaction "$2" 1); else dec=$(ecli getrawtransaction "$2" 1); fi
  local st="${3%%:*}" sv="${3##*:}"
  if ! echo "$dec" | jq -e --arg t "$st" --argjson v "$sv" \
      '.vin[] | select(.txid == $t and .vout == $v)' > /dev/null; then
    echo "✗ FAIL: $4 did not close seal $3"; exit 1
  fi
  echo "  ✓ seal closed: $4 spends $3"
}

# ── the two assets, each born hash-locked (the swap-1 offers) ───────
P1=$(openssl rand -hex 32); H1=$(printf '%s' "$P1" | xxd -r -p | shasum -a 256 | awk '{print $1}')
P2=$(openssl rand -hex 32); H2=$(printf '%s' "$P2" | xxd -r -p | shasum -a 256 | awk '{print $1}')

echo
echo "== Setup: oUSD genesis on a Bitcoin HTLC, lUSD genesis on a Liquid HTLC =="

HTLC_A1=$("$RGB_ANCHOR" htlc-address --hash "$H1" --claimer bob-btc --refund alice-btc --csv-delay $CSV_DELAY --hrp bcrt)
A1_ADDR=$(echo "$HTLC_A1" | jq -r '.address'); A1_SPK=$(echo "$HTLC_A1" | jq -r '.spk_hex')
A1_OUT=$(fund_htlc_btc "$A1_ADDR" "$A1_SPK")
echo "  BTC HTLC A1 (oUSD, claimer=Bob, H1): $A1_OUT"

HTLC_B1=$("$RGB_ANCHOR" htlc-address --hash "$H1" --claimer alice-liq --refund bob-liq --csv-delay $CSV_DELAY --hrp ert)
B1_ADDR=$(echo "$HTLC_B1" | jq -r '.address'); B1_SPK=$(echo "$HTLC_B1" | jq -r '.spk_hex')
B1_OUT=$(fund_htlc_liq "$B1_ADDR" "$B1_SPK")
echo "  LIQ HTLC B1 (lUSD, claimer=Alice, H1): $B1_OUT"

# Destination seals (real, wallet-controlled UTXOs — swap 2 must be
# able to close them).
read -r BOB_BTC_SEAL BOB_BTC_SPK   <<< "$(fund_btc)"
read -r ALICE_BTC_SEAL _           <<< "$(fund_btc)"
read -r ALICE_L_SEAL ALICE_L_SPK   <<< "$(fund_liq w_a)"
read -r BOB_L_SEAL _               <<< "$(fund_liq w_b)"

# ════════════════ SWAP 1: Alice's oUSD <-> Bob's lUSD ═══════════════
echo
echo "== Swap 1, leg 1: Alice claims lUSD on Liquid (reveals P1) =="
OUT=$("$RGB_ANCHOR" rgb20-transfer --name LiquidUSD --ticker lUSD \
  --supply $SUPPLY --send $SUPPLY --chain-net liquid-testnet \
  --alice-seal "$B1_OUT" --bob-seal "$ALICE_L_SEAL")
TAP_B1=$(echo "$OUT" | sed -n '1p'); AJ_B1="$OUT_DIR/anchor_rt_b1.json"
echo "$OUT" | sed -n '2p' > "$AJ_B1"; OPID_B1=$(echo "$OUT" | sed -n '3p')
TAP_B1_SPK=$(ecli validateaddress "$TAP_B1" | jq -r '.scriptPubKey')
DEST=$(ecli_w w_a getaddressinfo "$(ecli_w w_a getaddressinfo "$(ecli_w w_a getnewaddress)" | jq -r '.unconfidential')" | jq -r '.scriptPubKey')
CLAIM=$("$RGB_ANCHOR" htlc-spend-liquid \
  --prev-txid "${B1_OUT%%:*}" --prev-vout "${B1_OUT##*:}" --input-value-sat $FUND_SAT \
  --fee-sat $FEE_SAT --dest-spk "$DEST" --lbtc-asset "$LBTC" --hash "$H1" \
  --claimer alice-liq --refund bob-liq --csv-delay $CSV_DELAY \
  --branch claim --preimage "$P1" \
  --anchor-spk "$TAP_B1_SPK" --anchor-value-sat $ANCHOR_SAT)
TX_B1=$(ecli sendrawtransaction "$CLAIM"); ecli generatetoaddress 2 "$MINE_L" > /dev/null
jq --arg t "$TX_B1" '.txid = $t' "$AJ_B1" > "$AJ_B1.tmp" && mv "$AJ_B1.tmp" "$AJ_B1"
echo "  claim tx: $TX_B1 — P1 is now public"
assert_closes liq "$TX_B1" "$B1_OUT" "Alice's lUSD claim"
"$RGB_ANCHOR" verify --anchor "$AJ_B1"
"$RGB_ANCHOR" verify-patched --anchor "$AJ_B1"

echo
echo "== Swap 1, leg 2: Bob claims oUSD on Bitcoin with the revealed P1 =="
OUT=$("$RGB_ANCHOR" rgb20-transfer --name OnchainUSD --ticker oUSD \
  --supply $SUPPLY --send $SUPPLY --chain-net bitcoin-regtest \
  --alice-seal "$A1_OUT" --bob-seal "$BOB_BTC_SEAL")
TAP_A1=$(echo "$OUT" | sed -n '1p'); AJ_A1="$OUT_DIR/anchor_rt_a1.json"
echo "$OUT" | sed -n '2p' > "$AJ_A1"; OPID_A1=$(echo "$OUT" | sed -n '3p')
TAP_A1_SPK=$(bcli validateaddress "$TAP_A1" | jq -r '.scriptPubKey')
DEST=$(bcli_w w_btc getaddressinfo "$(bcli_w w_btc getnewaddress)" | jq -r '.scriptPubKey')
CLAIM=$("$RGB_ANCHOR" htlc-spend-btc \
  --prev-txid "${A1_OUT%%:*}" --prev-vout "${A1_OUT##*:}" --input-value-sat $FUND_SAT \
  --fee-sat $FEE_SAT --dest-spk "$DEST" --hash "$H1" \
  --claimer bob-btc --refund alice-btc --csv-delay $CSV_DELAY \
  --branch claim --preimage "$P1" \
  --anchor-spk "$TAP_A1_SPK" --anchor-value-sat $ANCHOR_SAT)
TX_A1=$(bcli sendrawtransaction "$CLAIM"); bcli_w w_btc generatetoaddress 2 "$MINE_B" > /dev/null
jq --arg t "$TX_A1" '.txid = $t' "$AJ_A1" > "$AJ_A1.tmp" && mv "$AJ_A1.tmp" "$AJ_A1"
echo "  claim tx: $TX_A1"
assert_closes btc "$TX_A1" "$A1_OUT" "Bob's oUSD claim"
"$RGB_ANCHOR" verify-patched --anchor "$AJ_A1" --bitcoin

echo
echo "  ➜ after swap 1: Bob owns oUSD on Bitcoin, Alice owns lUSD on Liquid."

# ════════════════ SWAP 2: the same swap, reversed ═══════════════════
echo
echo "== Swap 2, locks: both parties lock behind H2 (chained transfers) =="

HTLC_A2=$("$RGB_ANCHOR" htlc-address --hash "$H2" --claimer alice-btc --refund bob-btc --csv-delay $CSV_DELAY --hrp bcrt)
A2_ADDR=$(echo "$HTLC_A2" | jq -r '.address'); A2_SPK=$(echo "$HTLC_A2" | jq -r '.spk_hex')
A2_OUT=$(fund_htlc_btc "$A2_ADDR" "$A2_SPK")

# Bob's oUSD lock: transition consumes the allocation swap 1 created
# (OPID_A1), and its anchor tx closes Bob's seal on Bitcoin.
OUT=$("$RGB_ANCHOR" rgb20-transfer --name OnchainUSD --ticker oUSD \
  --supply $SUPPLY --send $SUPPLY --chain-net bitcoin-regtest \
  --alice-seal "$A1_OUT" --bob-seal "$A2_OUT" \
  --consume-opid "$OPID_A1" --prev-amount $SUPPLY --close-seal "$BOB_BTC_SEAL")
TAP_A2=$(echo "$OUT" | sed -n '1p'); AJ_A2="$OUT_DIR/anchor_rt_a2.json"
echo "$OUT" | sed -n '2p' > "$AJ_A2"; OPID_A2=$(echo "$OUT" | sed -n '3p')
RAW=$(bcli createrawtransaction \
  "[{\"txid\":\"${BOB_BTC_SEAL%%:*}\",\"vout\":${BOB_BTC_SEAL##*:}}]" \
  "[{\"$TAP_A2\":0.0001},{\"$(bcli_w w_btc getnewaddress)\":0.00085}]")
SIGNED=$(bcli_w w_btc signrawtransactionwithwallet "$RAW" | jq -r '.hex')
TX_A2=$(bcli sendrawtransaction "$SIGNED"); bcli_w w_btc generatetoaddress 2 "$MINE_B" > /dev/null
jq --arg t "$TX_A2" '.txid = $t' "$AJ_A2" > "$AJ_A2.tmp" && mv "$AJ_A2.tmp" "$AJ_A2"
echo "  BTC lock: oUSD -> HTLC A2 (claimer=Alice, H2), anchor tx $TX_A2"
assert_closes btc "$TX_A2" "$BOB_BTC_SEAL" "Bob's oUSD lock"
"$RGB_ANCHOR" verify-patched --anchor "$AJ_A2" --bitcoin

HTLC_B2=$("$RGB_ANCHOR" htlc-address --hash "$H2" --claimer bob-liq --refund alice-liq --csv-delay $CSV_DELAY --hrp ert)
B2_ADDR=$(echo "$HTLC_B2" | jq -r '.address'); B2_SPK=$(echo "$HTLC_B2" | jq -r '.spk_hex')
B2_OUT=$(fund_htlc_liq "$B2_ADDR" "$B2_SPK")

# Alice's lUSD lock, same construction on Liquid.
OUT=$("$RGB_ANCHOR" rgb20-transfer --name LiquidUSD --ticker lUSD \
  --supply $SUPPLY --send $SUPPLY --chain-net liquid-testnet \
  --alice-seal "$B1_OUT" --bob-seal "$B2_OUT" \
  --consume-opid "$OPID_B1" --prev-amount $SUPPLY --close-seal "$ALICE_L_SEAL")
TAP_B2=$(echo "$OUT" | sed -n '1p'); AJ_B2="$OUT_DIR/anchor_rt_b2.json"
echo "$OUT" | sed -n '2p' > "$AJ_B2"; OPID_B2=$(echo "$OUT" | sed -n '3p')
LCH=$(ecli_w w_a getaddressinfo "$(ecli_w w_a getnewaddress)" | jq -r '.unconfidential')
RAW=$(ecli createrawtransaction \
  "[{\"txid\":\"${ALICE_L_SEAL%%:*}\",\"vout\":${ALICE_L_SEAL##*:}}]" \
  "[{\"$TAP_B2\":0.0001,\"asset\":\"$LBTC\"},{\"$LCH\":0.00085,\"asset\":\"$LBTC\"},{\"fee\":0.00005,\"asset\":\"$LBTC\"}]")
SIGNED=$(ecli_w w_a signrawtransactionwithwallet "$RAW" | jq -r '.hex')
TX_B2=$(ecli sendrawtransaction "$SIGNED"); ecli generatetoaddress 2 "$MINE_L" > /dev/null
jq --arg t "$TX_B2" '.txid = $t' "$AJ_B2" > "$AJ_B2.tmp" && mv "$AJ_B2.tmp" "$AJ_B2"
echo "  LIQ lock: lUSD -> HTLC B2 (claimer=Bob, H2), anchor tx $TX_B2"
assert_closes liq "$TX_B2" "$ALICE_L_SEAL" "Alice's lUSD lock"
"$RGB_ANCHOR" verify --anchor "$AJ_B2"
"$RGB_ANCHOR" verify-patched --anchor "$AJ_B2"

echo
echo "== Swap 2, negative: yesterday's preimage does not unlock today's swap =="
DEST=$(bcli_w w_btc getaddressinfo "$(bcli_w w_btc getnewaddress)" | jq -r '.scriptPubKey')
BAD=$("$RGB_ANCHOR" htlc-spend-btc \
  --prev-txid "${A2_OUT%%:*}" --prev-vout "${A2_OUT##*:}" --input-value-sat $FUND_SAT \
  --fee-sat $FEE_SAT --dest-spk "$DEST" --hash "$H2" \
  --claimer alice-btc --refund bob-btc --csv-delay $CSV_DELAY \
  --branch claim --preimage "$P1")
if bcli sendrawtransaction "$BAD" 2>/dev/null; then
  echo "✗ FAIL: HTLC A2 claimed with the swap-1 preimage"; exit 1
fi
echo "  ✓ rejected: P1 does not satisfy H2"

echo
echo "== Swap 2, claims: Bob takes lUSD (reveals P2), Alice takes oUSD =="
OUT=$("$RGB_ANCHOR" rgb20-transfer --name LiquidUSD --ticker lUSD \
  --supply $SUPPLY --send $SUPPLY --chain-net liquid-testnet \
  --alice-seal "$B1_OUT" --bob-seal "$BOB_L_SEAL" \
  --consume-opid "$OPID_B2" --prev-amount $SUPPLY --close-seal "$B2_OUT")
TAP_B3=$(echo "$OUT" | sed -n '1p'); AJ_B3="$OUT_DIR/anchor_rt_b3.json"
echo "$OUT" | sed -n '2p' > "$AJ_B3"
TAP_B3_SPK=$(ecli validateaddress "$TAP_B3" | jq -r '.scriptPubKey')
DEST=$(ecli_w w_b getaddressinfo "$(ecli_w w_b getaddressinfo "$(ecli_w w_b getnewaddress)" | jq -r '.unconfidential')" | jq -r '.scriptPubKey')
CLAIM=$("$RGB_ANCHOR" htlc-spend-liquid \
  --prev-txid "${B2_OUT%%:*}" --prev-vout "${B2_OUT##*:}" --input-value-sat $FUND_SAT \
  --fee-sat $FEE_SAT --dest-spk "$DEST" --lbtc-asset "$LBTC" --hash "$H2" \
  --claimer bob-liq --refund alice-liq --csv-delay $CSV_DELAY \
  --branch claim --preimage "$P2" \
  --anchor-spk "$TAP_B3_SPK" --anchor-value-sat $ANCHOR_SAT)
TX_B3=$(ecli sendrawtransaction "$CLAIM"); ecli generatetoaddress 2 "$MINE_L" > /dev/null
jq --arg t "$TX_B3" '.txid = $t' "$AJ_B3" > "$AJ_B3.tmp" && mv "$AJ_B3.tmp" "$AJ_B3"
echo "  claim tx: $TX_B3 — P2 is now public"
assert_closes liq "$TX_B3" "$B2_OUT" "Bob's lUSD claim"
"$RGB_ANCHOR" verify --anchor "$AJ_B3"
"$RGB_ANCHOR" verify-patched --anchor "$AJ_B3"

OUT=$("$RGB_ANCHOR" rgb20-transfer --name OnchainUSD --ticker oUSD \
  --supply $SUPPLY --send $SUPPLY --chain-net bitcoin-regtest \
  --alice-seal "$A1_OUT" --bob-seal "$ALICE_BTC_SEAL" \
  --consume-opid "$OPID_A2" --prev-amount $SUPPLY --close-seal "$A2_OUT")
TAP_A3=$(echo "$OUT" | sed -n '1p'); AJ_A3="$OUT_DIR/anchor_rt_a3.json"
echo "$OUT" | sed -n '2p' > "$AJ_A3"
TAP_A3_SPK=$(bcli validateaddress "$TAP_A3" | jq -r '.scriptPubKey')
DEST=$(bcli_w w_btc getaddressinfo "$(bcli_w w_btc getnewaddress)" | jq -r '.scriptPubKey')
CLAIM=$("$RGB_ANCHOR" htlc-spend-btc \
  --prev-txid "${A2_OUT%%:*}" --prev-vout "${A2_OUT##*:}" --input-value-sat $FUND_SAT \
  --fee-sat $FEE_SAT --dest-spk "$DEST" --hash "$H2" \
  --claimer alice-btc --refund bob-btc --csv-delay $CSV_DELAY \
  --branch claim --preimage "$P2" \
  --anchor-spk "$TAP_A3_SPK" --anchor-value-sat $ANCHOR_SAT)
TX_A3=$(bcli sendrawtransaction "$CLAIM"); bcli_w w_btc generatetoaddress 2 "$MINE_B" > /dev/null
jq --arg t "$TX_A3" '.txid = $t' "$AJ_A3" > "$AJ_A3.tmp" && mv "$AJ_A3.tmp" "$AJ_A3"
echo "  claim tx: $TX_A3"
assert_closes btc "$TX_A3" "$A2_OUT" "Alice's oUSD claim"
"$RGB_ANCHOR" verify-patched --anchor "$AJ_A3" --bitcoin

echo
echo "✅ Done — the round trip closed."
echo "   oUSD: Bitcoin HTLC → Bob → HTLC → Alice. Never left Bitcoin."
echo "   lUSD: Liquid  HTLC → Alice → HTLC → Bob. Never left Liquid."
echo "   $SUPPLY of each, conserved through 3 transitions per asset;"
echo "   ownership crossed chains twice, atomically, with the swap as"
echo "   the router. Six anchors, all verified through the patched"
echo "   rgb-consensus — the same verify code on both chains."
