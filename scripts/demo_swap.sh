#!/usr/bin/env bash
#
# Cross-chain RGB atomic swap: kLUSD on Bitcoin  <->  kXAU on Liquid.
#
# Alice holds RGB20 kLUSD on Bitcoin. Bob holds RGB20 kXAU on Liquid.
# They swap, atomically, linked by a single SHA256 preimage.
#
#  1. Alice picks secret s, H = SHA256(s). Both chains get a P2WSH
#     hashlock OP_SHA256 <H> OP_EQUAL.
#  2. Leg 1 (Bitcoin): Alice issues kLUSD + builds a transfer; the
#     witness tx spends Alice's seal, anchors the RGB transition at
#     vout[0], and creates the hashlocked new seal at vout[1].
#  3. Leg 2 (Liquid): Bob does the mirror with kXAU.
#  4. Claim A (Liquid): Alice spends Bob's hashlock with s -> she gets
#     the kXAU seal. The spend tx publishes s on the Liquid chain.
#  5. Extract: Bob reads s off the Liquid claim tx's witness.
#  6. Claim B (Bitcoin): Bob spends Alice's hashlock with s -> kLUSD.
#  7. Both RGB anchors verified via the patched rgbcore::dbc::Anchor.
#
# Atomicity: Alice cannot claim kXAU without publishing s; once s is
# public, Bob can always claim kLUSD. Neither can take both.

set -euo pipefail
source "$(dirname "$0")/_lib.sh"
cd "$(cd "$(dirname "$0")/.." && pwd)"

RGB_ANCHOR="${RGB_ANCHOR:-./target/debug/rgb-anchor}"
[ -x "$RGB_ANCHOR" ] || cargo build -p spike-rgb-anchor >&2
LBTC="b2e15d0d7a0c94e4e2ce0fe6e8691b9e451377f6e46e8045a86f7c4b5d4f0f23"

echo "════════════════════════════════════════════════════════════"
echo "  cross-chain RGB atomic swap"
echo "  kLUSD (Bitcoin)  <->  kXAU (Liquid)"
echo "════════════════════════════════════════════════════════════"

# ── 1. Secret + hashlocks ───────────────────────────────────────────
SECRET=$(openssl rand -hex 32)
HL_BTC_JSON=$("$RGB_ANCHOR" swap-hashlock --preimage "$SECRET" --hrp bcrt)
HL_LQ_JSON=$("$RGB_ANCHOR"  swap-hashlock --preimage "$SECRET" --hrp ert)
H=$(echo "$HL_BTC_JSON" | jq -r '.hash_hex')
HL_BTC_ADDR=$(echo "$HL_BTC_JSON" | jq -r '.address')
HL_LQ_ADDR=$(echo "$HL_LQ_JSON"  | jq -r '.address')
echo
echo "  secret s (Alice only) : $SECRET"
echo "  hash   H (public)     : $H"
echo "  hashlock @ Bitcoin    : $HL_BTC_ADDR"
echo "  hashlock @ Liquid     : $HL_LQ_ADDR"

# ════════════════════════════════════════════════════════════════════
# LEG 1 — Bitcoin: Alice issues + transfers kLUSD
# ════════════════════════════════════════════════════════════════════
echo
echo "════════ LEG 1 — Bitcoin (kLUSD) ════════"

ALICE_UTXO=$(bcli_w w_btc listunspent 1 | jq -r '[.[] | select(.amount > 1)][0]')
ALICE_SEAL_TXID=$(echo "$ALICE_UTXO" | jq -r '.txid')
ALICE_SEAL_VOUT=$(echo "$ALICE_UTXO" | jq -r '.vout')
echo "  alice seal : $ALICE_SEAL_TXID:$ALICE_SEAL_VOUT"

OUT1=$("$RGB_ANCHOR" rgb20-transfer \
  --chain-net bitcoin-regtest \
  --name KaleidoUSD --ticker kLUSD --supply 1000000 --send 1000000 \
  --alice-seal "$ALICE_SEAL_TXID:$ALICE_SEAL_VOUT" \
  --bob-seal   "$(printf 'bob-klusd-receive' | shasum -a256 | awk '{print $1}'):1")
TAPRET1_ADDR=$(echo "$OUT1" | sed -n '1p')
ANCHOR1=$(echo "$OUT1" | sed -n '2p')
echo "$ANCHOR1" > "$OUT_DIR/swap_leg1.json"
echo "  tapret addr: $TAPRET1_ADDR"

# Witness tx: vin=alice seal, vout[0]=tapret, vout[1]=hashlock, change@2.
RAW1=$(bcli_w w_btc createrawtransaction \
  "[{\"txid\":\"$ALICE_SEAL_TXID\",\"vout\":$ALICE_SEAL_VOUT}]" \
  "[{\"$TAPRET1_ADDR\":0.0005},{\"$HL_BTC_ADDR\":0.0010}]")
FUND1=$(bcli_w w_btc fundrawtransaction "$RAW1" '{"changePosition":2,"add_inputs":true}')
SIGNED1=$(bcli_w w_btc signrawtransactionwithwallet "$(echo "$FUND1" | jq -r '.hex')")
LEG1_TXID=$(bcli_w w_btc sendrawtransaction "$(echo "$SIGNED1" | jq -r '.hex')")
bcli_w w_btc generatetoaddress 1 "$(bcli_w w_btc getnewaddress)" > /dev/null
echo "  leg-1 witness txid: $LEG1_TXID"
jq --arg t "$LEG1_TXID" '.txid=$t' "$OUT_DIR/swap_leg1.json" > "$OUT_DIR/swap_leg1.json.t" \
  && mv "$OUT_DIR/swap_leg1.json.t" "$OUT_DIR/swap_leg1.json"

# ════════════════════════════════════════════════════════════════════
# LEG 2 — Liquid: Bob issues + transfers kXAU
# ════════════════════════════════════════════════════════════════════
echo
echo "════════ LEG 2 — Liquid (kXAU) ════════"

BOB_UTXO=$(ecli_w w_b listunspent 1 | jq -r '[.[] | select(.asset=="'"$LBTC"'")][0]')
BOB_SEAL_TXID=$(echo "$BOB_UTXO" | jq -r '.txid')
BOB_SEAL_VOUT=$(echo "$BOB_UTXO" | jq -r '.vout')
echo "  bob seal   : $BOB_SEAL_TXID:$BOB_SEAL_VOUT"

OUT2=$("$RGB_ANCHOR" rgb20-transfer \
  --chain-net liquid-testnet \
  --name KaleidoGold --ticker kXAU --supply 10000 --send 10000 \
  --alice-seal "$BOB_SEAL_TXID:$BOB_SEAL_VOUT" \
  --bob-seal   "$(printf 'alice-kxau-receive' | shasum -a256 | awk '{print $1}'):1")
TAPRET2_ADDR=$(echo "$OUT2" | sed -n '1p')
ANCHOR2=$(echo "$OUT2" | sed -n '2p')
echo "$ANCHOR2" > "$OUT_DIR/swap_leg2.json"
echo "  tapret addr: $TAPRET2_ADDR"

RAW2=$(ecli_w w_b createrawtransaction \
  "[{\"txid\":\"$BOB_SEAL_TXID\",\"vout\":$BOB_SEAL_VOUT}]" \
  "[{\"$TAPRET2_ADDR\":0.0005,\"asset\":\"$LBTC\"},{\"$HL_LQ_ADDR\":0.0010,\"asset\":\"$LBTC\"}]")
FUND2=$(ecli_w w_b fundrawtransaction "$RAW2" '{"changePosition":2,"add_inputs":true}')
FUND2_HEX=$(echo "$FUND2" | jq -r '.hex')
BLIND2=$(ecli_w w_b blindrawtransaction "$FUND2_HEX" 2>/dev/null || echo "$FUND2_HEX")
SIGNED2=$(ecli_w w_b signrawtransactionwithwallet "$BLIND2")
LEG2_TXID=$(ecli sendrawtransaction "$(echo "$SIGNED2" | jq -r '.hex')")
ecli generatetoaddress 1 "$(ecli_w w_b getaddressinfo "$(ecli_w w_b getnewaddress)" | jq -r '.unconfidential')" > /dev/null
echo "  leg-2 witness txid: $LEG2_TXID"
jq --arg t "$LEG2_TXID" '.txid=$t' "$OUT_DIR/swap_leg2.json" > "$OUT_DIR/swap_leg2.json.t" \
  && mv "$OUT_DIR/swap_leg2.json.t" "$OUT_DIR/swap_leg2.json"

# ════════════════════════════════════════════════════════════════════
# CLAIM A — Alice claims kXAU on Liquid by revealing s
# ════════════════════════════════════════════════════════════════════
echo
echo "════════ CLAIM A — Alice claims kXAU (Liquid), revealing s ════════"

# Locate the hashlock output in leg-2's witness tx.
LEG2_TX=$(ecli getrawtransaction "$LEG2_TXID" 2)
HL_LQ_VOUT=$(echo "$LEG2_TX" | jq -r --arg spk "$(echo "$HL_LQ_JSON" | jq -r '.spk_hex')" \
  '.vout[] | select(.scriptPubKey.hex==$spk) | .n')
HL_LQ_VALUE=$(echo "$LEG2_TX" | jq -r --argjson n "$HL_LQ_VOUT" '.vout[$n].value')
HL_LQ_SAT=$(printf '%.0f' "$(echo "$HL_LQ_VALUE * 100000000" | bc -l)")
echo "  hashlock @ Liquid: $LEG2_TXID:$HL_LQ_VOUT  value=$HL_LQ_VALUE L-BTC"

ALICE_LQ_ADDR=$(ecli_w w_a getaddressinfo "$(ecli_w w_a getnewaddress)" | jq -r '.unconfidential')
ALICE_LQ_SPK=$(ecli_w w_a getaddressinfo "$ALICE_LQ_ADDR" | jq -r '.scriptPubKey')
CLAIM_A_FEE=500
CLAIM_A_OUT=$((HL_LQ_SAT - CLAIM_A_FEE))

CLAIM_A_RAW=$("$RGB_ANCHOR" swap-claim-liquid \
  --prev-txid "$LEG2_TXID" --prev-vout "$HL_LQ_VOUT" \
  --output-value-sat "$CLAIM_A_OUT" --fee-sat "$CLAIM_A_FEE" \
  --dest-spk "$ALICE_LQ_SPK" --lbtc-asset "$LBTC" --preimage "$SECRET")
CLAIM_A_TXID=$(ecli sendrawtransaction "$CLAIM_A_RAW")
ecli generatetoaddress 1 "$ALICE_LQ_ADDR" > /dev/null
echo "  ✓ Alice's claim tx: $CLAIM_A_TXID"
echo "    Alice now controls the kXAU seal. Secret s is now on-chain."

# ════════════════════════════════════════════════════════════════════
# EXTRACT — Bob reads s off the Liquid chain
# ════════════════════════════════════════════════════════════════════
echo
echo "════════ EXTRACT — Bob scrapes s from the Liquid claim tx ════════"
EXTRACTED=$(ecli getrawtransaction "$CLAIM_A_TXID" 2 \
  | jq -r '.vin[0].txinwitness[0]')
echo "  extracted preimage: $EXTRACTED"
if [ "$EXTRACTED" = "$SECRET" ]; then
  echo "  ✓ matches the secret Alice used — Bob can now claim kLUSD"
else
  echo "  ✗ FAIL: extracted preimage does not match"; exit 1
fi

# ════════════════════════════════════════════════════════════════════
# CLAIM B — Bob claims kLUSD on Bitcoin using the extracted s
# ════════════════════════════════════════════════════════════════════
echo
echo "════════ CLAIM B — Bob claims kLUSD (Bitcoin) with extracted s ════════"
LEG1_TX=$(bcli getrawtransaction "$LEG1_TXID" 2)
HL_BTC_VOUT=$(echo "$LEG1_TX" | jq -r --arg spk "$(echo "$HL_BTC_JSON" | jq -r '.spk_hex')" \
  '.vout[] | select(.scriptPubKey.hex==$spk) | .n')
HL_BTC_VALUE=$(echo "$LEG1_TX" | jq -r --argjson n "$HL_BTC_VOUT" '.vout[$n].value')
HL_BTC_SAT=$(printf '%.0f' "$(echo "$HL_BTC_VALUE * 100000000" | bc -l)")
echo "  hashlock @ Bitcoin: $LEG1_TXID:$HL_BTC_VOUT  value=$HL_BTC_VALUE BTC"

BOB_BTC_ADDR=$(bcli_w w_btc getnewaddress)
BOB_BTC_SPK=$(bcli_w w_btc getaddressinfo "$BOB_BTC_ADDR" | jq -r '.scriptPubKey')

CLAIM_B_RAW=$("$RGB_ANCHOR" swap-claim-btc \
  --prev-txid "$LEG1_TXID" --prev-vout "$HL_BTC_VOUT" \
  --input-value-sat "$HL_BTC_SAT" --fee-sat 500 \
  --dest-spk "$BOB_BTC_SPK" --preimage "$EXTRACTED")
CLAIM_B_TXID=$(bcli sendrawtransaction "$CLAIM_B_RAW")
bcli_w w_btc generatetoaddress 1 "$(bcli_w w_btc getnewaddress)" > /dev/null
echo "  ✓ Bob's claim tx: $CLAIM_B_TXID"
echo "    Bob now controls the kLUSD seal."

# ════════════════════════════════════════════════════════════════════
# VERIFY — both RGB anchors via the patched rgbcore::dbc::Anchor
# ════════════════════════════════════════════════════════════════════
echo
echo "════════ VERIFY — both RGB anchors (patched Anchor::verify) ════════"
echo "-- Leg 1 (Bitcoin):"
"$RGB_ANCHOR" verify-patched --anchor "$OUT_DIR/swap_leg1.json" --bitcoin
echo "-- Leg 2 (Liquid):"
"$RGB_ANCHOR" verify-patched --anchor "$OUT_DIR/swap_leg2.json"

echo
echo "✅ Done — cross-chain RGB atomic swap settled."
echo "   kLUSD (Bitcoin) and kXAU (Liquid) both moved, linked by one"
echo "   SHA256 preimage. Revealing s to claim leg 2 made s public,"
echo "   which let Bob claim leg 1. Atomic: neither party could take"
echo "   both sides."
