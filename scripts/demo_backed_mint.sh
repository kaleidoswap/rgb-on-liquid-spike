#!/usr/bin/env bash
#
# Backed mint on Liquid: the "LR-USDT" construction, end to end.
#
# An IFA (inflatable) RGB contract starts with zero circulating supply
# and its whole max supply as an inflation allowance on a *gate seal*.
# Minting is a TS_INFLATION transition anchored in a Liquid tx that:
#
#   vin[0]  : spends the gate seal            (closes it)
#   vout[0] : tapret commitment               (anchors the mint)
#   vout[1] : locks the backing asset         (the vault)
#   vout[2] : recipient's seal                (minted units live here)
#   vout[3] : next gate seal                  (remaining allowance)
#
# The backing rule: 1 minted unit per 1 unit (satoshi) of the backing
# asset locked to the vault. `verify-backed-mint` checks the anchor,
# the seal closure, AND the backing, all from the same witness tx.
# Elements outputs name their asset explicitly, so the check is a
# direct read of the transaction: no oracle.
#
# Bootstrap's test asset plays native USDt. The vault here is a fixed
# P2WSH; in production it is a Simplicity covenant.
#
# Proof points:
#   1. properly backed mint     → verifier accepts
#   2. mint with NO lock        → verifier rejects (locked 0)
#   3. under-locked mint        → verifier rejects (locked < required)

set -euo pipefail
source "$(dirname "$0")/_lib.sh"

cd "$(cd "$(dirname "$0")/.." && pwd)"

RGB_ANCHOR="${RGB_ANCHOR:-./target/debug/rgb-anchor}"
[ -x "$RGB_ANCHOR" ] || cargo build -p spike-rgb-anchor >&2

LBTC="b2e15d0d7a0c94e4e2ce0fe6e8691b9e451377f6e46e8045a86f7c4b5d4f0f23"
MAX_SUPPLY=1000000
MINT=250000                 # units minted
LOCK_COINS=0.00250000       # 250000 sat of the backing asset = 1:1
UNDER_COINS=0.00100000      # deliberately short for the negative test
FEE=0.00010000

# decimal-safe arithmetic
dsub() { python3 -c "from decimal import Decimal as D; print((D('$1')-D('$2')).quantize(D('0.00000001')))"; }

echo "════════════════════════════════════════════════════════════"
echo "  Backed mint: RGB dollars against a locked Elements asset"
echo "════════════════════════════════════════════════════════════"

BACKING=$(jq -r '.asset_id' out/asset.json)
echo "  backing asset (plays USDt): $BACKING"

# Raw explicit transactions cannot spend blinded inputs, and bootstrap
# delivered the asset to a confidential address. Self-send to an
# unconfidential address first so the demo has an explicit asset UTXO.
AEXP=$(ecli_w w_a getaddressinfo "$(ecli_w w_a getnewaddress)" | jq -r '.unconfidential')
ecli_w w_a sendtoaddress "$AEXP" 1.0 "" "" false false 1 "UNSET" false "$BACKING" > /dev/null
ecli generatetoaddress 1 "$(ecli_w w_a getaddressinfo "$(ecli_w w_a getnewaddress)" | jq -r '.unconfidential')" > /dev/null

# ── the vault: fixed P2WSH the backing must be locked to ────────────
VAULT_PRE=$(printf 'lr-usdt-vault-demo' | xxd -p -c 999)
VAULT=$("$RGB_ANCHOR" swap-hashlock --preimage "$VAULT_PRE" --hrp ert)
VAULT_ADDR=$(echo "$VAULT" | jq -r '.address')
VAULT_SPK=$(echo "$VAULT" | jq -r '.spk_hex')
echo "  vault: $VAULT_ADDR"

# ── three gate-seal UTXOs, one per mint attempt ─────────────────────
MINE=$(ecli_w w_a getaddressinfo "$(ecli_w w_a getnewaddress)" | jq -r '.unconfidential')
declare -a GATE_ADDR GATE_UTXO
FUND_OUTS="["
for i in 0 1 2; do
  GATE_ADDR[$i]=$(ecli_w w_a getaddressinfo "$(ecli_w w_a getnewaddress)" | jq -r '.unconfidential')
  FUND_OUTS+="{\"${GATE_ADDR[$i]}\": 0.01, \"asset\": \"$LBTC\"},"
done
FUND_OUTS="${FUND_OUTS%,}]"
FUND_TXID=$(ecli_w w_a sendmany "" "$(echo "$FUND_OUTS" | jq 'map(to_entries[0] | select(.key != "asset") | {(.key): .value}) | add')" 2>/dev/null) \
  || FUND_TXID=""
if [ -z "$FUND_TXID" ]; then
  # sendmany JSON gymnastics differ across versions; do three sends,
  # locking each gate UTXO immediately so wallet coin selection cannot
  # spend gate N while funding gate N+1.
  for i in 0 1 2; do
    T=$(ecli_w w_a sendtoaddress "${GATE_ADDR[$i]}" 0.01)
    V=$(ecli getrawtransaction "$T" 1 \
      | jq --arg a "${GATE_ADDR[$i]}" '.vout[] | select(.scriptPubKey.address == $a) | .n')
    GATE_UTXO[$i]="$T:$V"
    ecli_w w_a lockunspent false "[{\"txid\":\"$T\",\"vout\":$V}]" > /dev/null
  done
  ecli generatetoaddress 1 "$MINE" > /dev/null
else
  ecli generatetoaddress 1 "$MINE" > /dev/null
  for i in 0 1 2; do
    V=$(ecli getrawtransaction "$FUND_TXID" 1 \
      | jq --arg a "${GATE_ADDR[$i]}" '.vout[] | select(.scriptPubKey.address == $a) | .n')
    GATE_UTXO[$i]="$FUND_TXID:$V"
  done
fi
echo "  gate seals: ${GATE_UTXO[0]} ${GATE_UTXO[1]} ${GATE_UTXO[2]}"

# run_mint <n> <gate_utxo> <lock_coins|none> <anchor_file>
# Builds+broadcasts the witness tx for one mint attempt and patches
# the anchor. Output order is fixed by manual construction.
run_mint() {
  local n="$1" gate="$2" lock="$3" anchor_file="$4"
  local gate_txid="${gate%%:*}" gate_vout="${gate##*:}"

  local bob_ph change_ph
  bob_ph=$(printf 'backed-mint-%s-recipient' "$n" | shasum -a 256 | awk '{print $1}')
  change_ph=$(printf 'backed-mint-%s-gate2' "$n" | shasum -a 256 | awk '{print $1}')

  local out
  out=$("$RGB_ANCHOR" ifa-mint \
    --name LiquidRgbUSD --ticker LRUSD \
    --max-supply $MAX_SUPPLY --mint $MINT \
    --gate-seal "$gate" \
    --recipient-seal "$bob_ph:2" \
    --new-gate-seal "$change_ph:3")
  local addr anchor_json
  addr=$(echo "$out" | sed -n '1p')
  anchor_json=$(echo "$out" | sed -n '2p')
  echo "$anchor_json" > "$anchor_file"

  local bob gate2 lchange
  bob=$(ecli_w w_b getaddressinfo "$(ecli_w w_b getnewaddress)" | jq -r '.unconfidential')
  gate2=$(ecli_w w_a getaddressinfo "$(ecli_w w_a getnewaddress)" | jq -r '.unconfidential')
  lchange=$(ecli_w w_a getaddressinfo "$(ecli_w w_a getnewaddress)" | jq -r '.unconfidential')

  local inputs outputs
  if [ "$lock" = "none" ]; then
    # No backing input, no vault output. vout[1] becomes a dust L-BTC
    # output so the seal placeholders (vout 2, 3) keep their indexes.
    local dust
    dust=$(ecli_w w_a getaddressinfo "$(ecli_w w_a getnewaddress)" | jq -r '.unconfidential')
    inputs="[{\"txid\":\"$gate_txid\",\"vout\":$gate_vout}]"
    local lrest
    lrest=$(dsub "0.01" "0.0005"); lrest=$(dsub "$lrest" "0.0001")
    lrest=$(dsub "$lrest" "0.0003"); lrest=$(dsub "$lrest" "0.0003"); lrest=$(dsub "$lrest" "$FEE")
    outputs=$(jq -n --arg tap "$addr" --arg bob "$bob" --arg g2 "$gate2" --arg du "$dust" --arg ch "$lchange" \
      --arg lb "$LBTC" --arg rest "$lrest" --arg fee "$FEE" \
      '[ {($tap): 0.0005, "asset": $lb},
         {($du):  0.0001, "asset": $lb},
         {($bob): 0.0003, "asset": $lb},
         {($g2):  0.0003, "asset": $lb},
         {($ch):  ($rest|tonumber), "asset": $lb},
         {"fee":  ($fee|tonumber),  "asset": $lb} ]')
  else
    # Pick a backing-asset UTXO from w_a.
    local autxo a_txid a_vout a_amt achange arest
    autxo=$(ecli_w w_a listunspent 1 | jq --arg A "$BACKING" \
      '[.[] | select(.asset == $A and .amountblinder == "0000000000000000000000000000000000000000000000000000000000000000")][0]')
    [ "$autxo" != "null" ] || { echo "✗ no explicit backing-asset UTXO in w_a" >&2; exit 1; }
    a_txid=$(echo "$autxo" | jq -r '.txid'); a_vout=$(echo "$autxo" | jq -r '.vout')
    a_amt=$(echo "$autxo" | jq -r '.amount')
    achange=$(ecli_w w_a getaddressinfo "$(ecli_w w_a getnewaddress)" | jq -r '.unconfidential')
    arest=$(dsub "$a_amt" "$lock")

    inputs="[{\"txid\":\"$gate_txid\",\"vout\":$gate_vout},{\"txid\":\"$a_txid\",\"vout\":$a_vout}]"
    local lrest
    lrest=$(dsub "0.01" "0.0005"); lrest=$(dsub "$lrest" "0.0003")
    lrest=$(dsub "$lrest" "0.0003"); lrest=$(dsub "$lrest" "$FEE")
    outputs=$(jq -n --arg tap "$addr" --arg v "$VAULT_ADDR" --arg bob "$bob" --arg g2 "$gate2" \
      --arg ach "$achange" --arg lch "$lchange" --arg lb "$LBTC" --arg A "$BACKING" \
      --arg lock "$lock" --arg arest "$arest" --arg lrest "$lrest" --arg fee "$FEE" \
      '[ {($tap): 0.0005,            "asset": $lb},
         {($v):   ($lock|tonumber),  "asset": $A},
         {($bob): 0.0003,            "asset": $lb},
         {($g2):  0.0003,            "asset": $lb},
         {($ach): ($arest|tonumber), "asset": $A},
         {($lch): ($lrest|tonumber), "asset": $lb},
         {"fee":  ($fee|tonumber),   "asset": $lb} ]')
  fi

  local raw signed txid
  raw=$(ecli createrawtransaction "$inputs" "$outputs")
  signed=$(ecli_w w_a signrawtransactionwithwallet "$raw" | jq -r '.hex')
  txid=$(ecli sendrawtransaction "$signed")
  ecli generatetoaddress 2 "$MINE" > /dev/null
  jq --arg t "$txid" '.txid = $t' "$anchor_file" > "$anchor_file.tmp" && mv "$anchor_file.tmp" "$anchor_file"
  echo "$txid"
}

# ── 1. properly backed mint ─────────────────────────────────────────
echo
echo "== Mint 1: $MINT LRUSD backed by $LOCK_COINS of the asset =="
TX1=$(run_mint 1 "${GATE_UTXO[0]}" "$LOCK_COINS" "$OUT_DIR/anchor_backed_mint.json")
echo "  witness tx: $TX1"
echo
"$RGB_ANCHOR" verify-backed-mint \
  --anchor "$OUT_DIR/anchor_backed_mint.json" \
  --vault-spk "$VAULT_SPK" --backing-asset "$BACKING" --required $MINT

# ── 2. mint with NO lock at all ─────────────────────────────────────
echo
echo "== Mint 2 (negative): same mint, nothing locked =="
TX2=$(run_mint 2 "${GATE_UTXO[1]}" none "$OUT_DIR/anchor_unbacked_mint.json")
echo "  witness tx: $TX2"
if "$RGB_ANCHOR" verify-backed-mint \
    --anchor "$OUT_DIR/anchor_unbacked_mint.json" \
    --vault-spk "$VAULT_SPK" --backing-asset "$BACKING" --required $MINT 2>/dev/null; then
  echo "✗ FAIL: verifier accepted an unbacked mint"
  exit 1
fi
echo "✓ verifier rejected the unbacked mint"

# ── 3. under-locked mint ────────────────────────────────────────────
echo
echo "== Mint 3 (negative): $MINT LRUSD but only $UNDER_COINS locked =="
TX3=$(run_mint 3 "${GATE_UTXO[2]}" "$UNDER_COINS" "$OUT_DIR/anchor_underbacked_mint.json")
echo "  witness tx: $TX3"
if "$RGB_ANCHOR" verify-backed-mint \
    --anchor "$OUT_DIR/anchor_underbacked_mint.json" \
    --vault-spk "$VAULT_SPK" --backing-asset "$BACKING" --required $MINT 2>/dev/null; then
  echo "✗ FAIL: verifier accepted an under-backed mint"
  exit 1
fi
echo "✓ verifier rejected the under-backed mint"

echo
echo "✅ Done — backed minting on Liquid."
echo "   A real IFA (multi-mint) contract: supply starts as an inflation"
echo "   allowance on a gate seal, minting consumes it, and every holder"
echo "   can verify the backing of every mint straight from the witness"
echo "   transaction. Unbacked and under-backed mints are rejected."
