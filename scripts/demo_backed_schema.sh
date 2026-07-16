#!/usr/bin/env bash
#
# BFA — the backing-aware RGB schema, audited end to end.
#
# The backed-mint demo left one gap: the backing rule lived in the
# verifier's arguments, not in the contract. BFA closes it. The schema
# commits the backing terms (vault script, backing asset, rate) in
# genesis, so the terms are part of the contract id, and every mint's
# anchor chains back to them. `bfa-audit` then rebuilds the WHOLE
# claimed history and checks every mint against the chain:
#
#   * seal closure  — the witness tx spends the gate seal,
#   * anchor        — the witness tx commits to the rebuilt transition,
#   * backing       — the vault locked >= minted × rate of the asset.
#
# Proof points:
#   1. two honest chained mints            → audit PASSES
#   2. an over-mint the CHAIN ACCEPTS
#      (40k minted, 10k locked)            → audit FAILS (backing)
#   3. a history that lies about the size
#      of that mint to look backed         → audit FAILS (anchor)
#
# Point 2 is the thesis of client-side validation: the chain cannot
# read RGB amounts, so it happily confirms an under-backed mint; the
# schema-committed terms are what make every holder reject it.

set -euo pipefail
source "$(dirname "$0")/_lib.sh"

cd "$(cd "$(dirname "$0")/.." && pwd)"

RGB_ANCHOR="${RGB_ANCHOR:-./target/debug/rgb-anchor}"
[ -x "$RGB_ANCHOR" ] || cargo build -p spike-rgb-anchor >&2

LBTC="b2e15d0d7a0c94e4e2ce0fe6e8691b9e451377f6e46e8045a86f7c4b5d4f0f23"
MAX_SUPPLY=1000000
MINT1=30000;  LOCK1=0.00030000   # honest, 1:1
MINT2=20000;  LOCK2=0.00020000   # honest, chained
MINT3=40000;  LOCK3=0.00010000   # over-mint: 40k minted, 10k locked
FEE=0.00010000

dsub() { python3 -c "from decimal import Decimal as D; print((D('$1')-D('$2')).quantize(D('0.00000001')))"; }

echo "════════════════════════════════════════════════════════════"
echo "  BFA: a backing-aware RGB schema, with a full-history audit"
echo "════════════════════════════════════════════════════════════"

BACKING=$(jq -r '.asset_id' out/asset.json)
echo "  backing asset (plays USDt): $BACKING"

# Raw explicit transactions cannot spend blinded inputs; make sure w_a
# holds an explicit backing-asset UTXO.
AEXP=$(ecli_w w_a getaddressinfo "$(ecli_w w_a getnewaddress)" | jq -r '.unconfidential')
ecli_w w_a sendtoaddress "$AEXP" 1.0 "" "" false false 1 "UNSET" false "$BACKING" > /dev/null
MINE=$(ecli_w w_a getaddressinfo "$(ecli_w w_a getnewaddress)" | jq -r '.unconfidential')
ecli generatetoaddress 1 "$MINE" > /dev/null

# ── the vault, committed into the contract's genesis ────────────────
VAULT_PRE=$(printf 'bfa-vault-demo' | xxd -p -c 999)
VAULT=$("$RGB_ANCHOR" swap-hashlock --preimage "$VAULT_PRE" --hrp ert)
VAULT_ADDR=$(echo "$VAULT" | jq -r '.address')
VAULT_SPK=$(echo "$VAULT" | jq -r '.spk_hex')
TERMS="elements-backing:v1;vault=$VAULT_SPK;asset=$BACKING;rate=1/1"
echo "  vault: $VAULT_ADDR"
echo "  terms: committed in genesis, rate 1/1"

# ── gate seals: G0 (genesis), G1, G2, G3 (one per mint's next gate) ──
declare -a GATE_ADDR GATE_UTXO
for i in 0 1 2 3; do
  GATE_ADDR[$i]=$(ecli_w w_a getaddressinfo "$(ecli_w w_a getnewaddress)" | jq -r '.unconfidential')
  T=$(ecli_w w_a sendtoaddress "${GATE_ADDR[$i]}" 0.01)
  GATE_UTXO[$i]="$T"
done
ecli generatetoaddress 1 "$MINE" > /dev/null
for i in 0 1 2 3; do
  V=$(ecli getrawtransaction "${GATE_UTXO[$i]}" 1 \
    | jq --arg a "${GATE_ADDR[$i]}" '.vout[] | select(.scriptPubKey.address == $a) | .n')
  GATE_UTXO[$i]="${GATE_UTXO[$i]}:$V"
done

# run_bfa_mint <n> <mint_amount> <lock_coins> <gate> <new_gate> \
#              <consume_opid|-> <allowance|->
# Builds the BFA transition, broadcasts the witness tx (anchor + vault
# lock + seal closure), prints "txid opid".
run_bfa_mint() {
  local n="$1" amount="$2" lock="$3" gate="$4" new_gate="$5" copid="$6" allow="$7"
  local gate_txid="${gate%%:*}" gate_vout="${gate##*:}"

  local recip
  recip=$(printf 'bfa-mint-%s-recipient' "$n" | shasum -a 256 | awk '{print $1}')

  local out addr opid
  if [ "$copid" != "-" ]; then
    out=$("$RGB_ANCHOR" bfa-mint \
      --name LiquidRgbUSD --ticker LRUSD \
      --max-supply $MAX_SUPPLY --backing "$TERMS" \
      --mint "$amount" \
      --gate-seal "$gate" \
      --recipient-seal "$recip:2" \
      --new-gate-seal "$new_gate" \
      --orig-gate-seal "${GATE_UTXO[0]}" --consume-opid "$copid" --allowance "$allow")
  else
    out=$("$RGB_ANCHOR" bfa-mint \
      --name LiquidRgbUSD --ticker LRUSD \
      --max-supply $MAX_SUPPLY --backing "$TERMS" \
      --mint "$amount" \
      --gate-seal "$gate" \
      --recipient-seal "$recip:2" \
      --new-gate-seal "$new_gate")
  fi
  addr=$(echo "$out" | sed -n '1p')
  opid=$(echo "$out" | sed -n '3p')

  # Witness tx: gate seal + explicit asset UTXO in; anchor, vault
  # lock, recipient dust, asset change, L-BTC change, fee out.
  local autxo a_txid a_vout a_amt achange arest bob lchange lrest
  autxo=$(ecli_w w_a listunspent 1 | jq --arg A "$BACKING" \
    '[.[] | select(.asset == $A and .amountblinder == "0000000000000000000000000000000000000000000000000000000000000000")][0]')
  [ "$autxo" != "null" ] || { echo "✗ no explicit backing-asset UTXO in w_a" >&2; exit 1; }
  a_txid=$(echo "$autxo" | jq -r '.txid'); a_vout=$(echo "$autxo" | jq -r '.vout')
  a_amt=$(echo "$autxo" | jq -r '.amount')
  achange=$(ecli_w w_a getaddressinfo "$(ecli_w w_a getnewaddress)" | jq -r '.unconfidential')
  arest=$(dsub "$a_amt" "$lock")
  bob=$(ecli_w w_b getaddressinfo "$(ecli_w w_b getnewaddress)" | jq -r '.unconfidential')
  lchange=$(ecli_w w_a getaddressinfo "$(ecli_w w_a getnewaddress)" | jq -r '.unconfidential')
  lrest=$(dsub "0.01" "0.0005"); lrest=$(dsub "$lrest" "0.0003"); lrest=$(dsub "$lrest" "$FEE")

  local inputs outputs raw signed txid
  inputs="[{\"txid\":\"$gate_txid\",\"vout\":$gate_vout},{\"txid\":\"$a_txid\",\"vout\":$a_vout}]"
  outputs=$(jq -n --arg tap "$addr" --arg v "$VAULT_ADDR" --arg bob "$bob" \
    --arg ach "$achange" --arg lch "$lchange" --arg lb "$LBTC" --arg A "$BACKING" \
    --arg lock "$lock" --arg arest "$arest" --arg lrest "$lrest" --arg fee "$FEE" \
    '[ {($tap): 0.0005,            "asset": $lb},
       {($v):   ($lock|tonumber),  "asset": $A},
       {($bob): 0.0003,            "asset": $lb},
       {($ach): ($arest|tonumber), "asset": $A},
       {($lch): ($lrest|tonumber), "asset": $lb},
       {"fee":  ($fee|tonumber),   "asset": $lb} ]')
  raw=$(ecli createrawtransaction "$inputs" "$outputs")
  signed=$(ecli_w w_a signrawtransactionwithwallet "$raw" | jq -r '.hex')
  txid=$(ecli sendrawtransaction "$signed")
  ecli generatetoaddress 2 "$MINE" > /dev/null
  echo "$txid $opid"
}

RECIP() { printf 'bfa-mint-%s-recipient' "$1" | shasum -a 256 | awk '{print $1}'; }

# ── two honest chained mints ────────────────────────────────────────
echo
echo "== Mint 1: $MINT1 LRUSD, $LOCK1 locked (honest) =="
R1=$(run_bfa_mint 1 $MINT1 $LOCK1 "${GATE_UTXO[0]}" "${GATE_UTXO[1]}" - -)
TX1="${R1%% *}"; OPID1="${R1##* }"
echo "  witness tx: $TX1"

echo
echo "== Mint 2 (chained): $MINT2 LRUSD, $LOCK2 locked (honest) =="
R2=$(run_bfa_mint 2 $MINT2 $LOCK2 "${GATE_UTXO[1]}" "${GATE_UTXO[2]}" "$OPID1" $((MAX_SUPPLY - MINT1)))
TX2="${R2%% *}"; OPID2="${R2##* }"
echo "  witness tx: $TX2"

# ── audit the honest history ────────────────────────────────────────
mk_history() {  # <file> <mints_json>
  jq -n --arg backing "$TERMS" --arg g0 "${GATE_UTXO[0]}" --argjson mints "$2" \
    '{name:"LiquidRgbUSD", ticker:"LRUSD", max_supply:'"$MAX_SUPPLY"',
      backing:$backing, genesis_gate_seal:$g0,
      internal_key:"d6889cb081036e0faefa3a35157ad71086b123b2b144b649798b494c300a961d",
      entropy:12648430, mints:$mints}' > "$1"
}

echo
echo "== Audit: the honest two-mint history =="
mk_history "$OUT_DIR/bfa_history.json" "$(jq -n \
  --arg r1 "$(RECIP 1):2" --arg g1 "${GATE_UTXO[1]}" --arg t1 "$TX1" \
  --arg r2 "$(RECIP 2):2" --arg g2 "${GATE_UTXO[2]}" --arg t2 "$TX2" \
  '[{mint:'"$MINT1"', recipient_seal:$r1, new_gate_seal:$g1, witness_txid:$t1},
    {mint:'"$MINT2"', recipient_seal:$r2, new_gate_seal:$g2, witness_txid:$t2}]')"
"$RGB_ANCHOR" bfa-audit --history "$OUT_DIR/bfa_history.json"

# ── the over-mint: the chain accepts it ─────────────────────────────
echo
echo "== Mint 3 (over-mint): $MINT3 LRUSD minted, only $LOCK3 locked =="
R3=$(run_bfa_mint 3 $MINT3 $LOCK3 "${GATE_UTXO[2]}" "${GATE_UTXO[3]}" "$OPID2" $((MAX_SUPPLY - MINT1 - MINT2)))
TX3="${R3%% *}"
echo "  witness tx: $TX3 — CONFIRMED. The chain cannot see RGB amounts."

MINTS3() {  # <mint3_amount>
  jq -n \
    --arg r1 "$(RECIP 1):2" --arg g1 "${GATE_UTXO[1]}" --arg t1 "$TX1" \
    --arg r2 "$(RECIP 2):2" --arg g2 "${GATE_UTXO[2]}" --arg t2 "$TX2" \
    --arg r3 "$(RECIP 3):2" --arg g3 "${GATE_UTXO[3]}" --arg t3 "$TX3" \
    '[{mint:'"$MINT1"', recipient_seal:$r1, new_gate_seal:$g1, witness_txid:$t1},
      {mint:'"$MINT2"', recipient_seal:$r2, new_gate_seal:$g2, witness_txid:$t2},
      {mint:'"$1"',     recipient_seal:$r3, new_gate_seal:$g3, witness_txid:$t3}]'
}

echo
echo "== Audit: history including the over-mint =="
mk_history "$OUT_DIR/bfa_history_overmint.json" "$(MINTS3 $MINT3)"
if "$RGB_ANCHOR" bfa-audit --history "$OUT_DIR/bfa_history_overmint.json"; then
  echo "✗ FAIL: audit accepted an under-backed mint"; exit 1
fi
echo "✓ audit rejected the over-mint (backing rule)"

echo
echo "== Audit: history that LIES about mint 3's size (claims 10000) =="
mk_history "$OUT_DIR/bfa_history_lie.json" "$(MINTS3 10000)"
if "$RGB_ANCHOR" bfa-audit --history "$OUT_DIR/bfa_history_lie.json"; then
  echo "✗ FAIL: audit accepted a falsified history"; exit 1
fi
echo "✓ audit rejected the falsified history (anchor mismatch)"

echo
echo "✅ Done — a backing-aware RGB schema."
echo "   The backing terms are committed in the contract's genesis, so"
echo "   they travel with the contract id. The auditor rebuilds the whole"
echo "   mint history and checks every mint against the chain: honest"
echo "   history passes; an over-mint the chain happily confirmed fails"
echo "   the backing rule; and a history edited to hide it fails the"
echo "   anchor match. No oracle anywhere."
