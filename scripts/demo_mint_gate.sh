#!/usr/bin/env bash
#
# Simplicity mint-gate covenant: permissionless, rule-bound minting.
#
# The gate seal of an IFA contract (the UTXO carrying the inflation
# allowance) is locked under a Simplicity covenant instead of a key.
# ANYONE can spend it, but consensus only accepts a transaction shaped
# like a valid backed mint:
#
#   vout[0]  RGB opret anchor       OP_RETURN OP_PUSHBYTES_32 <root>
#   vout[1]  exact backing tranche  locked to the vault (asset+amount)
#   vout[2]  recipient's new seal   (unconstrained)
#   vout[3]  the gate, re-created    under the SAME covenant (recursion)
#
# The chain enforces the container; RGB's client-side validation (the
# IFA schema + the backed-mint verifier) enforces the contents. Nobody,
# not even the operator, sits in the mint path.
#
# Runs against the Simplicity-enabled node (Elements 23.3, port 7042).
# Two chained mints prove the recursion: mint #2 spends the gate that
# mint #1 created. Then three consensus negatives.

set -euo pipefail
source "$(dirname "$0")/_lib.sh"

cd "$(cd "$(dirname "$0")/.." && pwd)"

SIMP="${SIMP:-./target/debug/simp}"
RGB_ANCHOR="${RGB_ANCHOR:-./target/debug/rgb-anchor}"
[ -x "$SIMP" ] || cargo build -p spike-simplicity >&2
[ -x "$RGB_ANCHOR" ] || cargo build -p spike-rgb-anchor >&2

GATE_PROG="crates/spike-simplicity/programs/mint_gate_covenant.simf"
MAX_SUPPLY=1000000
TRANCHE=250000              # units minted per round = asset-sats locked
GATE_SAT=20000             # L-BTC carried on the gate UTXO
RECIP_SAT=5000
FEE_SAT=2000
# rgb-anchor verifiers read ELEMENTSD_RPC_URL; point them at the
# Simplicity node so verify-backed-mint fetches from the right chain.
export ELEMENTSD_RPC_URL="http://localhost:7042"

# elements-cli against the Simplicity node.
scli()   { $COMPOSE exec -T elementsd-simplicity elements-cli -chain="$CHAIN" -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASS" -rpcport=7042 "$@"; }
scli_w() { local w="$1"; shift; $COMPOSE exec -T elementsd-simplicity elements-cli -chain="$CHAIN" -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASS" -rpcport=7042 -rpcwallet="$w" "$@"; }

dsub() { python3 -c "from decimal import Decimal as D; print((D('$1')-D('$2')).quantize(D('0.00000001')))"; }
btc()  { python3 -c "from decimal import Decimal as D; print((D('$1')/D('100000000')).quantize(D('0.00000001')))"; }

echo "════════════════════════════════════════════════════════════"
echo "  Simplicity mint-gate covenant: permissionless backed minting"
echo "════════════════════════════════════════════════════════════"

ACTIVE=$(scli getdeploymentinfo | jq -r '.deployments.simplicity.active')
[ "$ACTIVE" = "true" ] || { echo "✗ simplicity not active on elementsd-simplicity" >&2; exit 1; }
GENESIS=$(scli getblockhash 0)
LBTC=$(scli dumpassetlabels | jq -r '.bitcoin')
echo "  simplicity : ACTIVE   L-BTC: ${LBTC:0:16}…"

# ── wallet + backing asset ──────────────────────────────────────────
scli createwallet w_g >/dev/null 2>&1 || scli loadwallet w_g >/dev/null 2>&1 || true
W=$(scli_w w_g getaddressinfo "$(scli_w w_g getnewaddress)" | jq -r '.unconfidential')
if [ "$(scli_w w_g listunspent 0 | jq 'length')" -eq 0 ]; then
  echo "== Seed w_g from the OP_TRUE genesis coins =="
  SCAN=$(scli scantxoutset start '["raw(51)"]')
  GTX=$(echo "$SCAN" | jq -r '.unspents[0].txid'); GVO=$(echo "$SCAN" | jq -r '.unspents[0].vout')
  OUTS=$(jq -n --arg w "$W" --arg a "$LBTC" '[{($w):50,"asset":$a},{"fee":20999950,"asset":$a}]')
  RAW=$(scli createrawtransaction "[{\"txid\":\"$GTX\",\"vout\":$GVO}]" "$OUTS")
  scli sendrawtransaction "$RAW" 0 >/dev/null
  scli generatetoaddress 2 "$W" >/dev/null
fi
# Issue a backing asset (plays native USDt) and make it explicit.
ISSUE=$(scli_w w_g issueasset 100 0)
BACKING=$(echo "$ISSUE" | jq -r '.asset')
scli generatetoaddress 1 "$W" >/dev/null
BEXP=$(scli_w w_g getaddressinfo "$(scli_w w_g getnewaddress)" | jq -r '.unconfidential')
scli_w w_g sendtoaddress "$BEXP" 50 "" "" false false 1 "UNSET" false "$BACKING" >/dev/null
scli generatetoaddress 1 "$W" >/dev/null
echo "  backing asset (plays USDt): ${BACKING:0:16}…"

# ── the vault + minter demo key ─────────────────────────────────────
VAULT=$("$RGB_ANCHOR" swap-hashlock --preimage "$(printf 'lr-usdt-vault' | xxd -p -c 99)" --hrp ert)
VAULT_SPK=$(echo "$VAULT" | jq -r '.spk_hex')
VAULT_HASH=$(printf '%s' "$VAULT_SPK" | xxd -r -p | shasum -a 256 | awk '{print $1}')
KEY=$("$SIMP" demo-address --label minter)
KEY_ADDR=$(echo "$KEY" | jq -r '.address')

# covenant args: vault hash, backing asset, exact tranche.
# `jet::output_asset` returns the asset id in internal (consensus) byte
# order, which is the reverse of the display hex `issueasset` prints, so
# the covenant param must be byte-reversed.
# byte-reverse the display hex (portable: `tail -r` is BSD-only)
BACKING_LE=$(python3 -c "s='$BACKING'; print(''.join(s[i:i+2] for i in range(len(s)-2,-1,-2)))")
ARGS="$OUT_DIR/mint_gate_args.json"
jq -n --arg v "0x$VAULT_HASH" --arg a "0x$BACKING_LE" --arg t "$TRANCHE" \
  '{VAULT_SPK_HASH:{value:$v,type:"u256"},BACKING_ASSET:{value:$a,type:"u256"},TRANCHE:{value:($t),type:"u64"}}' \
  > "$ARGS"
GATE=$("$SIMP" address --program "$GATE_PROG" --args "$ARGS")
GATE_ADDR=$(scli getdeploymentinfo >/dev/null; echo "$GATE" | jq -r '.address')
GATE_SPK=$(echo "$GATE" | jq -r '.spk_hex')
echo "  mint-gate covenant: $GATE_ADDR"
echo "  vault: $(echo "$VAULT" | jq -r '.address')"

# fund_gate: create the covenant UTXO carrying $GATE_SAT L-BTC.
fund_gate() {
  local txid vout
  txid=$(scli_w w_g sendtoaddress "$GATE_ADDR" "$(btc $GATE_SAT)")
  scli generatetoaddress 1 "$W" >/dev/null
  vout=$(scli getrawtransaction "$txid" 1 | jq --arg s "$GATE_SPK" '.vout[] | select(.scriptPubKey.hex==$s) | .n')
  echo "$txid:$vout"
}

# fund_key <asset> <amount_sat>: pay the minter key a precise UTXO.
fund_key() {
  local asset="$1" sat="$2" txid vout
  if [ "$asset" = "$LBTC" ]; then
    txid=$(scli_w w_g sendtoaddress "$KEY_ADDR" "$(btc $sat)")
  else
    txid=$(scli_w w_g sendtoaddress "$KEY_ADDR" "$(btc $sat)" "" "" false false 1 "UNSET" false "$asset")
  fi
  scli generatetoaddress 1 "$W" >/dev/null
  local spk
  spk=$(echo "$KEY" | jq -r '.spk_hex')
  vout=$(scli getrawtransaction "$txid" 1 | jq --arg s "$spk" '.vout[] | select(.scriptPubKey.hex==$s) | .n')
  echo "$txid:$vout"
}

# recipient seal spk (any wallet address; unconstrained by the covenant)
RECIP_SPK=$(scli_w w_g getaddressinfo "$(scli_w w_g getaddressinfo "$(scli_w w_g getnewaddress)" | jq -r '.unconfidential')" | jq -r '.scriptPubKey')

# assert_backing <spend_txid>: the covenant already enforced anchor +
# backing + gate-recreation at CONSENSUS level (the mint would not have
# been mined otherwise). This re-reads the vault output to show the
# backing that consensus required is really there. The anchor is opret
# here (a plain OP_RETURN the covenant can constrain by shape), so the
# tapret-oriented `verify-backed-mint` path does not apply.
assert_backing() {
  local txid="$1" dec locked
  dec=$(scli getrawtransaction "$txid" 1)
  locked=$(echo "$dec" | jq --arg s "$VAULT_SPK" --arg a "$BACKING" \
    '[.vout[] | select(.scriptPubKey.hex==$s and .asset==$a) | .value] | add // 0')
  # value is in whole units; tranche is asset-sats. 250000 sat = 0.0025.
  local want; want=$(btc $TRANCHE)
  if [ "$(python3 -c "from decimal import Decimal as D; print(D('$locked')>=D('$want'))")" = "True" ]; then
    echo "  ✓ backing verified on-chain: $locked of the asset locked to the vault (>= $want)"
  else
    echo "  ✗ FAIL: vault holds $locked, expected >= $want"; exit 1
  fi
}

# do_mint <round> <gate_utxo> <consume_opid|GENESIS> <allowance> <orig_gate> <tamper> <anchor_file>
# Returns "spend_txid gate_out_txid:vout new_opid" (gate_out only when accepted & untampered).
do_mint() {
  local round="$1" gate="$2" opid="$3" allow="$4" orig="$5" tamper="$6" anchor_file="$7"
  local gate_txid="${gate%%:*}" gate_vout="${gate##*:}"
  # Future recipient (vout 2) and re-created gate (vout 3) seals live in
  # the witness tx we are about to build; use a per-round placeholder
  # txid (same technique as demo_backed_mint.sh / demo_rgb20.sh).
  local newgate
  newgate=$(printf 'mint-gate-round-%s' "$round" | shasum -a 256 | awk '{print $1}')

  # 1. RGB side: mint transition + anchor JSON. new-gate-seal is the
  #    FUTURE covenant output (vout 3) of the tx we are about to build.
  local out addr opid_new
  if [ "$opid" = "GENESIS" ]; then
    out=$("$RGB_ANCHOR" ifa-mint --name LiquidRgbUSD --ticker LRUSD \
      --max-supply $MAX_SUPPLY --mint $TRANCHE \
      --gate-seal "$gate" --recipient-seal "$newgate:2" --new-gate-seal "$newgate:3")
  else
    out=$("$RGB_ANCHOR" ifa-mint --name LiquidRgbUSD --ticker LRUSD \
      --max-supply $MAX_SUPPLY --mint $TRANCHE \
      --gate-seal "$gate" --recipient-seal "$newgate:2" --new-gate-seal "$newgate:3" \
      --consume-opid "$opid" --allowance "$allow" --orig-gate-seal "$orig")
  fi
  addr=$(echo "$out" | sed -n '1p')
  echo "$out" | sed -n '2p' > "$anchor_file"
  opid_new=$(echo "$out" | sed -n '3p')
  local root
  root=$(scli validateaddress "$addr" | jq -r '.scriptPubKey' | cut -c5-)  # opret payload = MPC root; taken from anchor

  # MPC root is the tapret tweak target; the covenant commits to the
  # SAME 32-byte payload as an opret. Pull it from the anchor JSON.
  root=$(jq -r '.mpc_root_hex' "$anchor_file")

  # 2. Fund the minter: one asset UTXO of exactly the tranche, one
  #    L-BTC UTXO for recipient+gate+change+fee.
  local need=$((RECIP_SAT + GATE_SAT + FEE_SAT + 5000))
  local a_utxo f_utxo
  a_utxo=$(fund_key "$BACKING" "$TRANCHE")
  f_utxo=$(fund_key "$LBTC" "$need")

  # 3. Simplicity side: build+satisfy+sign the witness tx.
  local raw
  raw=$("$SIMP" mint-spend --program "$GATE_PROG" --args "$ARGS" \
    --anchor-payload "$root" \
    --gate-txid "$gate_txid" --gate-vout "$gate_vout" --gate-value-sat $GATE_SAT \
    --asset-txid "${a_utxo%%:*}" --asset-vout "${a_utxo##*:}" \
    --fee-txid "${f_utxo%%:*}" --fee-vout "${f_utxo##*:}" --fee-input-sat $need \
    --key-label minter --vault-spk "$VAULT_SPK" \
    --backing-asset "$BACKING" --tranche $TRANCHE \
    --recipient-spk "$RECIP_SPK" --recipient-sat $RECIP_SAT --fee-sat $FEE_SAT \
    --lbtc-asset "$LBTC" --genesis-hash "$GENESIS" --tamper "$tamper")

  if [ "$tamper" != "none" ]; then
    if scli sendrawtransaction "$raw" >/dev/null 2>&1; then
      echo "TAMPER_ACCEPTED"
    else
      echo "TAMPER_REJECTED"
    fi
    return
  fi

  local txid
  txid=$(scli sendrawtransaction "$raw")
  scli generatetoaddress 1 "$W" >/dev/null
  jq --arg t "$txid" '.txid=$t' "$anchor_file" > "$anchor_file.tmp" && mv "$anchor_file.tmp" "$anchor_file"
  # the recreated gate is vout 3 of this very tx.
  echo "$txid $txid:3 $opid_new"
}

# ── ROUND 1: first permissionless mint (consumes the genesis gate) ──
echo
echo "════════ ROUND 1 — mint against the genesis gate ════════"
GATE1=$(fund_gate)
echo "  gate UTXO: $GATE1"
read -r TX1 GATE2 OPID1 < <(do_mint 1 "$GATE1" GENESIS $MAX_SUPPLY "$GATE1" none "$OUT_DIR/anchor_gate_mint1.json")
echo "  ✓ mint tx: $TX1"
echo "  recreated gate: $GATE2"
assert_backing "$TX1"

# ── ROUND 2: chained mint spends the gate ROUND 1 created ───────────
echo
echo "════════ ROUND 2 — mint against the gate round 1 re-created ════════"
echo "  spending recreated gate: $GATE2"
REMAIN=$((MAX_SUPPLY - TRANCHE))
read -r TX2 GATE3 OPID2 < <(do_mint 2 "$GATE2" "$OPID1" $REMAIN "$GATE1" none "$OUT_DIR/anchor_gate_mint2.json")
echo "  ✓ mint tx: $TX2"
echo "  recreated gate: $GATE3"
assert_backing "$TX2"

# ── NEGATIVES: consensus must refuse malformed mints ────────────────
echo
echo "════════ NEGATIVES — consensus rejects malformed mints ════════"
for MODE in drop-anchor wrong-amount no-recreate; do
  GN=$(fund_gate)
  R=$(do_mint "9$MODE" "$GN" GENESIS $MAX_SUPPLY "$GN" "$MODE" "$OUT_DIR/anchor_gate_neg.json")
  if [ "$R" = "TAMPER_REJECTED" ]; then
    echo "  ✓ $MODE → rejected by consensus"
  else
    echo "  ✗ FAIL: $MODE was accepted"; exit 1
  fi
done

echo
echo "✅ Done — permissionless minting through a Simplicity covenant."
echo "   Two chained mints settled: each spent a gate the previous mint"
echo "   re-created, locked the backing, and anchored the RGB transition,"
echo "   with no key in the mint path. Malformed mints (no anchor, short"
echo "   backing, no gate re-creation) are refused by consensus itself."
