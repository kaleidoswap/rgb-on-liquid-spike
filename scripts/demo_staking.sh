#!/usr/bin/env bash
#
# Time-locked staking covenant on Liquid, via Simplicity.
#
# A staked RGB position (the seal holding the principal) is locked under
# a covenant that consensus only lets you spend once:
#
#   * the maturity height has passed        (absolute time lock),
#   * output 0 carries the RGB opret anchor  (the return is anchored),
#   * output 1 pays the staker's script      (principal returns home).
#
# The covenant is keyless: after maturity anyone may trigger the unstake,
# but the principal can only go back to the staker. The reward for having
# staked is an RGB issuance layered on top (composes with the mint gate);
# the covenant here enforces the lock and the destination.
#
# Runs against the Simplicity node (Elements 23.3, port 7042). Proof:
#   1. stake:   lock the principal under the covenant
#   2. early:   unstake before maturity        -> rejected (time lock)
#   3. wrong:   unstake to the wrong address    -> rejected (covenant)
#   4. no-anchor: unstake without the anchor    -> rejected (covenant)
#   5. mature:  unstake after maturity, correct -> accepted, funds home

set -euo pipefail
source "$(dirname "$0")/_lib.sh"

cd "$(cd "$(dirname "$0")/.." && pwd)"

SIMP="${SIMP:-./target/debug/simp}"
RGB_ANCHOR="${RGB_ANCHOR:-./target/debug/rgb-anchor}"
[ -x "$SIMP" ] || cargo build -p spike-simplicity >&2
[ -x "$RGB_ANCHOR" ] || cargo build -p spike-rgb-anchor >&2

PROG="crates/spike-simplicity/programs/staking_covenant.simf"
STAKE_SAT=15000        # L-BTC carried on the staked seal
FEE_SAT=1500
export ELEMENTSD_RPC_URL="http://localhost:7042"

scli()   { $COMPOSE exec -T elementsd-simplicity elements-cli -chain="$CHAIN" -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASS" -rpcport=7042 "$@"; }
scli_w() { local w="$1"; shift; $COMPOSE exec -T elementsd-simplicity elements-cli -chain="$CHAIN" -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASS" -rpcport=7042 -rpcwallet="$w" "$@"; }
btc()  { python3 -c "from decimal import Decimal as D; print((D('$1')/D('100000000')).quantize(D('0.00000001')))"; }

echo "════════════════════════════════════════════════════════════"
echo "  Time-locked staking covenant (Simplicity)"
echo "════════════════════════════════════════════════════════════"

ACTIVE=$(scli getdeploymentinfo | jq -r '.deployments.simplicity.active')
[ "$ACTIVE" = "true" ] || { echo "✗ simplicity not active" >&2; exit 1; }
GENESIS=$(scli getblockhash 0)
LBTC=$(scli dumpassetlabels | jq -r '.bitcoin')

scli createwallet w_s >/dev/null 2>&1 || scli loadwallet w_s >/dev/null 2>&1 || true
W=$(scli_w w_s getaddressinfo "$(scli_w w_s getnewaddress)" | jq -r '.unconfidential')
if [ "$(scli_w w_s listunspent 0 | jq 'length')" -eq 0 ]; then
  echo "== Seed w_s from the OP_TRUE genesis coins =="
  SCAN=$(scli scantxoutset start '["raw(51)"]')
  GTX=$(echo "$SCAN" | jq -r '.unspents[0].txid'); GVO=$(echo "$SCAN" | jq -r '.unspents[0].vout')
  OUTS=$(jq -n --arg w "$W" --arg a "$LBTC" '[{($w):50,"asset":$a},{"fee":20999950,"asset":$a}]')
  RAW=$(scli createrawtransaction "[{\"txid\":\"$GTX\",\"vout\":$GVO}]" "$OUTS")
  scli sendrawtransaction "$RAW" 0 >/dev/null
  scli generatetoaddress 2 "$W" >/dev/null
fi

# The staker's return address, and the demo key that pays the unstake fee.
STAKER_SPK=$(scli_w w_s getaddressinfo "$(scli_w w_s getaddressinfo "$(scli_w w_s getnewaddress)" | jq -r '.unconfidential')" | jq -r '.scriptPubKey')
STAKER_HASH=$(printf '%s' "$STAKER_SPK" | xxd -r -p | shasum -a 256 | awk '{print $1}')
KEY=$("$SIMP" demo-address --label poker)
KEY_ADDR=$(echo "$KEY" | jq -r '.address')
KEY_SPK=$(echo "$KEY" | jq -r '.spk_hex')

# Maturity = now + margin (margin covers the funding blocks below).
H=$(scli getblockcount)
MATURITY=$((H + 20))
echo "  current height : $H"
echo "  maturity height: $MATURITY"

ARGS="$OUT_DIR/staking_args.json"
jq -n --arg m "$MATURITY" --arg s "0x$STAKER_HASH" \
  '{MATURITY_HEIGHT:{value:$m,type:"Height"},STAKER_SPK_HASH:{value:$s,type:"u256"}}' > "$ARGS"
STAKE=$("$SIMP" address --program "$PROG" --args "$ARGS")
STAKE_ADDR=$(echo "$STAKE" | jq -r '.address')
STAKE_SPK=$(echo "$STAKE" | jq -r '.spk_hex')
echo "  staking covenant: $STAKE_ADDR"

# ── 1. Stake: lock the principal under the covenant ─────────────────
echo
echo "== Stake: lock the principal into the covenant seal =="
STK_TXID=$(scli_w w_s sendtoaddress "$STAKE_ADDR" "$(btc $STAKE_SAT)")
scli generatetoaddress 1 "$W" >/dev/null
STK_VOUT=$(scli getrawtransaction "$STK_TXID" 1 | jq --arg s "$STAKE_SPK" '.vout[] | select(.scriptPubKey.hex==$s) | .n')
echo "  staked seal: $STK_TXID:$STK_VOUT ($STAKE_SAT sat + the RGB principal)"

# The unstake transition's anchor root (a real RGB20 transfer root).
NEWSEAL=$(printf 'unstake-return' | shasum -a 256 | awk '{print $1}')
OUT=$("$RGB_ANCHOR" rgb20-transfer --name StakedRgbUSD --ticker sRUSD \
  --supply 1000000 --send 1000000 \
  --alice-seal "$STK_TXID:$STK_VOUT" --bob-seal "$NEWSEAL:1" 2>/dev/null)
ROOT=$(echo "$OUT" | sed -n '2p' | jq -r '.mpc_root_hex')

# Fund the fee key.
fund_fee() {
  local need=$((STAKE_SAT + FEE_SAT + 5000)) txid vout
  txid=$(scli_w w_s sendtoaddress "$KEY_ADDR" "$(btc $need)")
  scli generatetoaddress 1 "$W" >/dev/null
  vout=$(scli getrawtransaction "$txid" 1 | jq --arg s "$KEY_SPK" '.vout[] | select(.scriptPubKey.hex==$s) | .n')
  echo "$txid:$vout:$need"
}

build_unstake() {  # <tamper> <dest_spk>
  local tamper="$1" dest="$2" f
  f=$(fund_fee)
  "$SIMP" unstake-spend --program "$PROG" --args "$ARGS" \
    --anchor-payload "$ROOT" \
    --stake-txid "$STK_TXID" --stake-vout "$STK_VOUT" --stake-value-sat $STAKE_SAT \
    --fee-txid "${f%%:*}" --fee-vout "$(echo "$f" | cut -d: -f2)" --fee-input-sat "${f##*:}" \
    --key-label poker --staker-spk "$dest" --principal-sat $STAKE_SAT \
    --maturity-height $MATURITY --fee-sat $FEE_SAT \
    --lbtc-asset "$LBTC" --genesis-hash "$GENESIS" --tamper "$tamper"
}

# ── 2. Early unstake: before maturity → consensus rejects ───────────
echo
echo "== Unstake #1 (early): before maturity height =="
EARLY=$(build_unstake none "$STAKER_SPK")
if OUT=$(scli sendrawtransaction "$EARLY" 2>&1); then
  echo "✗ FAIL: early unstake accepted at height $(scli getblockcount)"; exit 1
fi
echo "  ✓ rejected by consensus (time lock not matured)"
echo "    └─ $(echo "$OUT" | head -1)"

echo
echo "== Mining to the maturity height ($MATURITY) =="
CUR=$(scli getblockcount)
[ "$CUR" -lt "$MATURITY" ] && scli generatetoaddress $((MATURITY - CUR)) "$W" >/dev/null
echo "  height now: $(scli getblockcount)"

# ── 3. Wrong destination: covenant rejects ─────────────────────────
echo
echo "== Unstake #2 (wrong destination): principal not returning to staker =="
WRONG=$(build_unstake wrong-dest "$STAKER_SPK")
if scli sendrawtransaction "$WRONG" >/dev/null 2>&1; then
  echo "✗ FAIL: wrong-destination unstake accepted"; exit 1
fi
echo "  ✓ rejected by the covenant"

# ── 4. Dropped anchor: covenant rejects ────────────────────────────
echo
echo "== Unstake #3 (no anchor): return transition not anchored =="
NOANCHOR=$(build_unstake drop-anchor "$STAKER_SPK")
if scli sendrawtransaction "$NOANCHOR" >/dev/null 2>&1; then
  echo "✗ FAIL: anchor-less unstake accepted"; exit 1
fi
echo "  ✓ rejected by the covenant"

# ── 5. Mature, correct unstake: accepted ───────────────────────────
echo
echo "== Unstake #4 (mature, correct): principal returns to the staker =="
GOOD=$(build_unstake none "$STAKER_SPK")
UNSTAKE_TXID=$(scli sendrawtransaction "$GOOD")
scli generatetoaddress 1 "$W" >/dev/null
echo "  ✓ accepted: $UNSTAKE_TXID"
DEC=$(scli getrawtransaction "$UNSTAKE_TXID" 1)
DEST=$(echo "$DEC" | jq -r '.vout[1].scriptPubKey.hex')
[ "$DEST" = "$STAKER_SPK" ] || { echo "✗ FAIL: principal did not return to the staker"; exit 1; }
echo "  ✓ principal returned to the staker at vout[1], anchored at vout[0]"

echo
echo "✅ Done — time-locked staking on Liquid."
echo "   The staked position could not be unstaked before maturity, could"
echo "   not be redirected away from the staker, and could not skip the"
echo "   RGB anchor. After maturity the correct unstake settled, all"
echo "   enforced by consensus with no key in the path."
