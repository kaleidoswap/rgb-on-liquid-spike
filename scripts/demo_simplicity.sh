#!/usr/bin/env bash
#
# Simplicity covenant on Liquid regtest — "preimage(H) ∧ anchor-shaped output".
#
# Runs against the elementsd-simplicity service (Elements 23.3, the
# Simplicity tapleaf deployment force-activated). The covenant program
# (crates/spike-simplicity/programs/rgb_anchor_covenant.simf) locks a
# coin so it can ONLY be spent by a transaction that:
#
#   1. reveals the preimage of H            (the atomic-swap link), AND
#   2. carries at vout 0 an output shaped exactly like an RGB `opret`
#      commitment: OP_RETURN OP_PUSHBYTES_32 <32 bytes>.
#
# (2) is the part Bitcoin/Elements Script cannot express and Simplicity
# can: the CHAIN enforces that the spend carries an RGB-anchor-shaped
# commitment output. This is the "preimage(H) ∧ valid_rgb_anchor"
# covenant sketched in the KaleidoSwap Liquid plan, reduced to running
# code.
#
# Proof points:
#   A. wrong preimage        → the program cannot even be satisfied
#   B. anchor output stripped AFTER satisfaction → CONSENSUS rejects
#      (proves the covenant is enforced by the chain, not by our tool)
#   C. compliant spend       → accepted; vout 0 is the opret commitment

set -euo pipefail
source "$(dirname "$0")/_lib.sh"

cd "$(cd "$(dirname "$0")/.." && pwd)"

SIMP="${SIMP:-./target/debug/simp}"
[ -x "$SIMP" ] || cargo build -p spike-simplicity >&2

PROGRAM="crates/spike-simplicity/programs/rgb_anchor_covenant.simf"
FEE_SAT=1000
FUND_AMT=0.001
FUND_SAT=100000

# elements-cli against the SIMPLICITY node (port 7042).
scli() {
  $COMPOSE exec -T elementsd-simplicity \
    elements-cli -chain="$CHAIN" -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASS" \
    -rpcport=7042 "$@"
}
scli_w() {
  local wallet="$1"; shift
  $COMPOSE exec -T elementsd-simplicity \
    elements-cli -chain="$CHAIN" -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASS" \
    -rpcport=7042 -rpcwallet="$wallet" "$@"
}

echo "════════════════════════════════════════════════════════════"
echo "  Simplicity covenant: preimage(H) ∧ anchor-shaped output"
echo "════════════════════════════════════════════════════════════"

# ── 0. Node sanity: Simplicity must be active ───────────────────────
ACTIVE=$(scli getdeploymentinfo | jq -r '.deployments.simplicity.active')
if [ "$ACTIVE" != "true" ]; then
  echo "✗ FAIL: simplicity deployment not active on elementsd-simplicity"
  exit 1
fi
GENESIS=$(scli getblockhash 0)
LBTC=$(scli dumpassetlabels | jq -r '.bitcoin')
echo "  simplicity : ACTIVE"
echo "  genesis    : $GENESIS"
echo "  L-BTC      : $LBTC"

# ── 1. Bootstrap a wallet from the OP_TRUE genesis coins (idempotent) ──
scli createwallet w_s > /dev/null 2>&1 || scli loadwallet w_s > /dev/null 2>&1 || true
W_CT=$(scli_w w_s getnewaddress)
W_ADDR=$(scli_w w_s getaddressinfo "$W_CT" | jq -r '.unconfidential')

SPENDABLE=$(scli_w w_s listunspent 0 | jq 'length')
if [ "$SPENDABLE" -eq 0 ]; then
  echo
  echo "== Seeding w_s from the OP_TRUE genesis output =="
  SCAN=$(scli scantxoutset start '["raw(51)"]')
  GEN_TXID=$(echo "$SCAN" | jq -r '.unspents[0].txid')
  GEN_VOUT=$(echo "$SCAN" | jq -r '.unspents[0].vout')
  OUTPUTS=$(jq -n --arg w "$W_ADDR" --arg asset "$LBTC" \
    '[ {($w): 10, "asset": $asset}, {"fee": 20999990, "asset": $asset} ]')
  RAW=$(scli createrawtransaction "[{\"txid\":\"$GEN_TXID\",\"vout\":$GEN_VOUT}]" "$OUTPUTS")
  scli sendrawtransaction "$RAW" 0 > /dev/null
  scli generatetoaddress 2 "$W_ADDR" > /dev/null
  echo "  w_s balance: $(scli_w w_s getbalance | jq -r '.bitcoin') L-BTC"
fi

# ── 2. Derive the covenant address for a fresh hashlock ────────────
PREIMAGE=$(openssl rand -hex 32)
HASH=$(printf '%s' "$PREIMAGE" | xxd -r -p | shasum -a 256 | awk '{print $1}')
# The RGB opret payload the spender will commit to (stands in for a
# real MPC root at spend time).
MPC_ROOT=$(printf 'rgb-mpc-root-%s' "$PREIMAGE" | shasum -a 256 | awk '{print $1}')

ARGS_FILE="$OUT_DIR/simplicity_args.json"
jq -n --arg h "0x$HASH" \
  '{ "EXPECTED_HASH": { "value": $h, "type": "u256" } }' > "$ARGS_FILE"

COV=$("$SIMP" address --program "$PROGRAM" --args "$ARGS_FILE")
COV_ADDR=$(echo "$COV" | jq -r '.address')
COV_SPK=$(echo "$COV" | jq -r '.spk_hex')
CMR=$(echo "$COV" | jq -r '.cmr')
echo
echo "== Covenant =="
echo "  program : $PROGRAM"
echo "  CMR     : $CMR"
echo "  address : $COV_ADDR (taproot leaf 0xbe)"

# ── 3. Fund the covenant UTXO ───────────────────────────────────────
echo
echo "== Fund the covenant UTXO =="
FUND_TXID=$(scli_w w_s sendtoaddress "$COV_ADDR" $FUND_AMT)
scli generatetoaddress 1 "$W_ADDR" > /dev/null
FUND_VOUT=$(scli getrawtransaction "$FUND_TXID" 1 \
  | jq --arg spk "$COV_SPK" '.vout[] | select(.scriptPubKey.hex == $spk) | .n')
echo "  covenant UTXO: $FUND_TXID:$FUND_VOUT ($FUND_SAT sat)"

DEST_SPK=$(scli_w w_s getaddressinfo "$W_ADDR" | jq -r '.scriptPubKey')

WIT_FILE="$OUT_DIR/simplicity_witness.json"
jq -n --arg p "0x$PREIMAGE" --arg m "0x$MPC_ROOT" \
  '{ "PREIMAGE":       { "value": $p, "type": "u256" },
     "ANCHOR_PAYLOAD": { "value": $m, "type": "u256" } }' > "$WIT_FILE"

# ── A. Negative: wrong preimage cannot even satisfy the program ─────
echo
echo "-- Negative A: wrong preimage --"
WRONG=$(openssl rand -hex 32)
BAD_WIT="$OUT_DIR/simplicity_witness_bad.json"
jq --arg p "0x$WRONG" '.PREIMAGE.value = $p' "$WIT_FILE" > "$BAD_WIT"
if "$SIMP" spend --program "$PROGRAM" --args "$ARGS_FILE" --witness "$BAD_WIT" \
    --prev-txid "$FUND_TXID" --prev-vout "$FUND_VOUT" --input-value-sat $FUND_SAT \
    --dest-spk "$DEST_SPK" --fee-sat $FEE_SAT --lbtc-asset "$LBTC" \
    --genesis-hash "$GENESIS" --opret-payload "$MPC_ROOT" 2>/dev/null; then
  echo "✗ FAIL: program satisfied with a wrong preimage"
  exit 1
fi
echo "  ✓ program refuses to satisfy (assertion fails in execution)"

# ── B. Negative: strip the anchor output AFTER satisfaction ─────────
# The witness is computed against the compliant tx, then the opret
# output is removed. Only the CHAIN can catch this one.
echo
echo "-- Negative B: anchor output stripped after satisfaction --"
TAMPERED=$("$SIMP" spend --program "$PROGRAM" --args "$ARGS_FILE" --witness "$WIT_FILE" \
  --prev-txid "$FUND_TXID" --prev-vout "$FUND_VOUT" --input-value-sat $FUND_SAT \
  --dest-spk "$DEST_SPK" --fee-sat $FEE_SAT --lbtc-asset "$LBTC" \
  --genesis-hash "$GENESIS" --opret-payload "$MPC_ROOT" --tamper-drop-anchor)
if OUT=$(scli sendrawtransaction "$TAMPERED" 2>&1); then
  echo "✗ FAIL: chain accepted a spend without the anchor output: $OUT"
  exit 1
fi
echo "  ✓ CONSENSUS rejected the anchor-less spend"
echo "    └─ $(echo "$OUT" | head -1)"

# ── C. Positive: compliant spend ────────────────────────────────────
echo
echo "-- Positive C: compliant spend (preimage + opret anchor) --"
GOOD=$("$SIMP" spend --program "$PROGRAM" --args "$ARGS_FILE" --witness "$WIT_FILE" \
  --prev-txid "$FUND_TXID" --prev-vout "$FUND_VOUT" --input-value-sat $FUND_SAT \
  --dest-spk "$DEST_SPK" --fee-sat $FEE_SAT --lbtc-asset "$LBTC" \
  --genesis-hash "$GENESIS" --opret-payload "$MPC_ROOT")
SPEND_TXID=$(scli sendrawtransaction "$GOOD")
scli generatetoaddress 1 "$W_ADDR" > /dev/null
echo "  ✓ accepted: $SPEND_TXID"

DECODED=$(scli getrawtransaction "$SPEND_TXID" 1)
OPRET_SPK=$(echo "$DECODED" | jq -r '.vout[0].scriptPubKey.hex')
if [ "$OPRET_SPK" != "6a20$MPC_ROOT" ]; then
  echo "✗ FAIL: vout[0] is not the expected opret commitment"
  exit 1
fi
echo "  ✓ vout[0] = OP_RETURN OP_PUSHBYTES_32 <MPC root> (6a20${MPC_ROOT:0:16}…)"

echo
echo "✅ Done — Simplicity covenant enforced ON-CHAIN:"
echo "   spends of the seal UTXO must reveal the preimage AND carry an"
echo "   RGB-anchor-shaped commitment output. A spend stripped of its"
echo "   anchor was rejected by consensus, not by tooling."
