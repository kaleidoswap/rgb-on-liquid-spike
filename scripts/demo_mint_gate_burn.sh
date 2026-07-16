#!/usr/bin/env bash
#
# Simplicity mint-gate covenant, BURN variant: permissionless minting
# backed by a provable burn instead of a vault lock.
#
# Consensus accepts a mint only if the transaction:
#
#   vout[0]  is BOTH the RGB opret anchor AND the asset burn:
#            OP_RETURN OP_PUSHBYTES_32 <root>, carrying TRANCHE of the
#            backing asset to an unspendable output (destroyed).
#   vout[1]  recipient's new seal   (unconstrained)
#   vout[2]  the gate, re-created    under the SAME covenant (recursion)
#
# Versus demo_mint_gate.sh (the lock variant) there is no vault: the
# backing is burned, not held. Proof-of-reserves becomes proof-of-burn.
#
# Two chained mints prove the recursion; three consensus negatives.

set -euo pipefail
source "$(dirname "$0")/_lib.sh"

cd "$(cd "$(dirname "$0")/.." && pwd)"

SIMP="${SIMP:-./target/debug/simp}"
RGB_ANCHOR="${RGB_ANCHOR:-./target/debug/rgb-anchor}"
[ -x "$SIMP" ] || cargo build -p spike-simplicity >&2
[ -x "$RGB_ANCHOR" ] || cargo build -p spike-rgb-anchor >&2

GATE_PROG="crates/spike-simplicity/programs/mint_gate_burn_covenant.simf"
MAX_SUPPLY=1000000
TRANCHE=250000
GATE_SAT=20000
RECIP_SAT=5000
FEE_SAT=2000
export ELEMENTSD_RPC_URL="http://localhost:7042"

scli()   { $COMPOSE exec -T elementsd-simplicity elements-cli -chain="$CHAIN" -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASS" -rpcport=7042 "$@"; }
scli_w() { local w="$1"; shift; $COMPOSE exec -T elementsd-simplicity elements-cli -chain="$CHAIN" -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASS" -rpcport=7042 -rpcwallet="$w" "$@"; }
btc()  { python3 -c "from decimal import Decimal as D; print((D('$1')/D('100000000')).quantize(D('0.00000001')))"; }

echo "════════════════════════════════════════════════════════════"
echo "  Mint-gate covenant (BURN): permissionless minting via burn"
echo "════════════════════════════════════════════════════════════"

ACTIVE=$(scli getdeploymentinfo | jq -r '.deployments.simplicity.active')
[ "$ACTIVE" = "true" ] || { echo "✗ simplicity not active" >&2; exit 1; }
GENESIS=$(scli getblockhash 0)
LBTC=$(scli dumpassetlabels | jq -r '.bitcoin')
echo "  simplicity : ACTIVE   L-BTC: ${LBTC:0:16}…"

# wallet + backing asset
scli createwallet w_b >/dev/null 2>&1 || scli loadwallet w_b >/dev/null 2>&1 || true
W=$(scli_w w_b getaddressinfo "$(scli_w w_b getnewaddress)" | jq -r '.unconfidential')
if [ "$(scli_w w_b listunspent 0 | jq 'length')" -eq 0 ]; then
  echo "== Seed w_b from the OP_TRUE genesis coins =="
  SCAN=$(scli scantxoutset start '["raw(51)"]')
  GTX=$(echo "$SCAN" | jq -r '.unspents[0].txid'); GVO=$(echo "$SCAN" | jq -r '.unspents[0].vout')
  OUTS=$(jq -n --arg w "$W" --arg a "$LBTC" '[{($w):50,"asset":$a},{"fee":20999950,"asset":$a}]')
  RAW=$(scli createrawtransaction "[{\"txid\":\"$GTX\",\"vout\":$GVO}]" "$OUTS")
  scli sendrawtransaction "$RAW" 0 >/dev/null
  scli generatetoaddress 2 "$W" >/dev/null
fi
ISSUE=$(scli_w w_b issueasset 100 0)
BACKING=$(echo "$ISSUE" | jq -r '.asset')
scli generatetoaddress 1 "$W" >/dev/null
BEXP=$(scli_w w_b getaddressinfo "$(scli_w w_b getnewaddress)" | jq -r '.unconfidential')
scli_w w_b sendtoaddress "$BEXP" 50 "" "" false false 1 "UNSET" false "$BACKING" >/dev/null
scli generatetoaddress 1 "$W" >/dev/null
echo "  backing asset (plays USDt): ${BACKING:0:16}…"

# minter demo key
KEY=$("$SIMP" demo-address --label minter)
KEY_ADDR=$(echo "$KEY" | jq -r '.address')
KEY_SPK=$(echo "$KEY" | jq -r '.spk_hex')

# covenant args (no vault): backing asset byte-reversed to internal
# order (as jet::output_asset returns it), and the exact tranche.
BACKING_LE=$(printf '%s' "$BACKING" | fold -w2 | tail -r | tr -d '\n')
ARGS="$OUT_DIR/mint_gate_burn_args.json"
jq -n --arg a "0x$BACKING_LE" --arg t "$TRANCHE" \
  '{BACKING_ASSET:{value:$a,type:"u256"},TRANCHE:{value:($t),type:"u64"}}' > "$ARGS"
GATE=$("$SIMP" address --program "$GATE_PROG" --args "$ARGS")
GATE_ADDR=$(echo "$GATE" | jq -r '.address')
GATE_SPK=$(echo "$GATE" | jq -r '.spk_hex')
echo "  burn mint-gate covenant: $GATE_ADDR"

fund_gate() {
  local txid vout
  txid=$(scli_w w_b sendtoaddress "$GATE_ADDR" "$(btc $GATE_SAT)")
  scli generatetoaddress 1 "$W" >/dev/null
  vout=$(scli getrawtransaction "$txid" 1 | jq --arg s "$GATE_SPK" '.vout[] | select(.scriptPubKey.hex==$s) | .n')
  echo "$txid:$vout"
}
fund_key() {
  local asset="$1" sat="$2" txid vout
  if [ "$asset" = "$LBTC" ]; then
    txid=$(scli_w w_b sendtoaddress "$KEY_ADDR" "$(btc $sat)")
  else
    txid=$(scli_w w_b sendtoaddress "$KEY_ADDR" "$(btc $sat)" "" "" false false 1 "UNSET" false "$asset")
  fi
  scli generatetoaddress 1 "$W" >/dev/null
  vout=$(scli getrawtransaction "$txid" 1 | jq --arg s "$KEY_SPK" '.vout[] | select(.scriptPubKey.hex==$s) | .n')
  echo "$txid:$vout"
}

RECIP_SPK=$(scli_w w_b getaddressinfo "$(scli_w w_b getaddressinfo "$(scli_w w_b getnewaddress)" | jq -r '.unconfidential')" | jq -r '.scriptPubKey')

# assert_burn <spend_txid>: confirm the tranche of the backing asset was
# sent to an OP_RETURN output (destroyed). Consensus already required it;
# this re-reads it for the demo. OP_RETURN scriptPubKey starts with 6a.
assert_burn() {
  local txid="$1" dec burned
  dec=$(scli getrawtransaction "$txid" 1)
  burned=$(echo "$dec" | jq --arg a "$BACKING" \
    '[.vout[] | select(.asset==$a and (.scriptPubKey.hex | startswith("6a"))) | .value] | add // 0')
  local want; want=$(btc $TRANCHE)
  if [ "$(python3 -c "from decimal import Decimal as D; print(D('$burned')>=D('$want'))")" = "True" ]; then
    echo "  ✓ burn verified on-chain: $burned of the asset destroyed to OP_RETURN (>= $want)"
  else
    echo "  ✗ FAIL: burned $burned, expected >= $want"; exit 1
  fi
}

# do_mint <round> <gate_utxo> <consume_opid|GENESIS> <allowance> <orig_gate> <tamper> <anchor_file>
do_mint() {
  local round="$1" gate="$2" opid="$3" allow="$4" orig="$5" tamper="$6" anchor_file="$7"
  local gate_txid="${gate%%:*}" gate_vout="${gate##*:}"
  local newgate; newgate=$(printf 'burn-gate-round-%s' "$round" | shasum -a 256 | awk '{print $1}')

  # RGB side. recipient seal = vout 1, re-created gate = vout 2 (burn layout).
  local out addr
  if [ "$opid" = "GENESIS" ]; then
    out=$("$RGB_ANCHOR" ifa-mint --name LiquidRgbUSD --ticker LRUSD \
      --max-supply $MAX_SUPPLY --mint $TRANCHE \
      --gate-seal "$gate" --recipient-seal "$newgate:1" --new-gate-seal "$newgate:2")
  else
    out=$("$RGB_ANCHOR" ifa-mint --name LiquidRgbUSD --ticker LRUSD \
      --max-supply $MAX_SUPPLY --mint $TRANCHE \
      --gate-seal "$gate" --recipient-seal "$newgate:1" --new-gate-seal "$newgate:2" \
      --consume-opid "$opid" --allowance "$allow" --orig-gate-seal "$orig")
  fi
  addr=$(echo "$out" | sed -n '1p')
  echo "$out" | sed -n '2p' > "$anchor_file"
  local opid_new; opid_new=$(echo "$out" | sed -n '3p')
  local root; root=$(jq -r '.mpc_root_hex' "$anchor_file")

  local need=$((RECIP_SAT + GATE_SAT + FEE_SAT + 5000))
  local a_utxo f_utxo
  a_utxo=$(fund_key "$BACKING" "$TRANCHE")
  f_utxo=$(fund_key "$LBTC" "$need")

  local raw
  raw=$("$SIMP" mint-spend-burn --program "$GATE_PROG" --args "$ARGS" \
    --anchor-payload "$root" \
    --gate-txid "$gate_txid" --gate-vout "$gate_vout" --gate-value-sat $GATE_SAT \
    --asset-txid "${a_utxo%%:*}" --asset-vout "${a_utxo##*:}" \
    --fee-txid "${f_utxo%%:*}" --fee-vout "${f_utxo##*:}" --fee-input-sat $need \
    --key-label minter --backing-asset "$BACKING" --tranche $TRANCHE \
    --recipient-spk "$RECIP_SPK" --recipient-sat $RECIP_SAT --fee-sat $FEE_SAT \
    --lbtc-asset "$LBTC" --genesis-hash "$GENESIS" --tamper "$tamper")

  if [ "$tamper" != "none" ]; then
    if scli sendrawtransaction "$raw" >/dev/null 2>&1; then echo "TAMPER_ACCEPTED"; else echo "TAMPER_REJECTED"; fi
    return
  fi
  local txid; txid=$(scli sendrawtransaction "$raw")
  scli generatetoaddress 1 "$W" >/dev/null
  echo "$txid $txid:2 $opid_new"
}

echo
echo "════════ ROUND 1 — burn-mint against the genesis gate ════════"
GATE1=$(fund_gate); echo "  gate UTXO: $GATE1"
read -r TX1 GATE2 OPID1 < <(do_mint 1 "$GATE1" GENESIS $MAX_SUPPLY "$GATE1" none "$OUT_DIR/anchor_burn_mint1.json")
echo "  ✓ mint tx: $TX1"
echo "  recreated gate: $GATE2"
assert_burn "$TX1"

echo
echo "════════ ROUND 2 — burn-mint against the gate round 1 re-created ════════"
echo "  spending recreated gate: $GATE2"
REMAIN=$((MAX_SUPPLY - TRANCHE))
read -r TX2 GATE3 OPID2 < <(do_mint 2 "$GATE2" "$OPID1" $REMAIN "$GATE1" none "$OUT_DIR/anchor_burn_mint2.json")
echo "  ✓ mint tx: $TX2"
echo "  recreated gate: $GATE3"
assert_burn "$TX2"

echo
echo "════════ NEGATIVES — consensus rejects malformed burn-mints ════════"
for MODE in drop-anchor wrong-amount no-recreate; do
  GN=$(fund_gate)
  R=$(do_mint "9$MODE" "$GN" GENESIS $MAX_SUPPLY "$GN" "$MODE" "$OUT_DIR/anchor_burn_neg.json")
  if [ "$R" = "TAMPER_REJECTED" ]; then echo "  ✓ $MODE → rejected by consensus"; else echo "  ✗ FAIL: $MODE accepted"; exit 1; fi
done

echo
echo "✅ Done — permissionless minting via burn."
echo "   Each mint destroyed the backing tranche to an OP_RETURN that is"
echo "   simultaneously the RGB anchor, re-created the covenant for the"
echo "   next minter, and needed no vault and no key. Malformed burn-mints"
echo "   are refused by consensus."
