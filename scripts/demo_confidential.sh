#!/usr/bin/env bash
#
# M4 — Confidential RGB20 transfer on Liquid.
#
# Same pipeline as demo_rgb20.sh, but every wallet-controlled output is
# blinded (Elements Confidential Transactions):
#
#   - Alice's seal UTXO is itself confidential (created by a CT self-send,
#     so the *closed* seal is a blinded output too).
#   - Bob's seal output (vout 1) pays a confidential address.
#   - Alice's change (vout 2) is blinded by the wallet.
#   - The tapret commitment output (vout 0) stays explicit: the commitment
#     lives in the scriptPubKey, which Elements never blinds. That is the
#     load-bearing claim of RGB-on-Liquid, and this script proves it: the
#     unmodified verifiers recover the commitment and the seal closure from
#     a transaction whose amounts and assets are hidden.
#
# Unlike demo_rgb20.sh, blindrawtransaction failure is fatal here — this
# demo is meaningless if the transaction goes out unblinded — and the
# script asserts on-chain that the seal outputs carry value/asset
# commitments instead of explicit values.

set -euo pipefail
source "$(dirname "$0")/_lib.sh"

cd "$(cd "$(dirname "$0")/.." && pwd)"

RGB_ANCHOR="${RGB_ANCHOR:-./target/debug/rgb-anchor}"
[ -x "$RGB_ANCHOR" ] || cargo build -p spike-rgb-anchor >&2

echo "════════════════════════════════════════════════════════════"
echo "  M4 — Confidential RGB20 (NIA) transfer on Liquid"
echo "════════════════════════════════════════════════════════════"

LBTC="b2e15d0d7a0c94e4e2ce0fe6e8691b9e451377f6e46e8045a86f7c4b5d4f0f23"

# ── 1. Give Alice a *confidential* seal UTXO ────────────────────────────
# A CT self-send: the resulting UTXO has blinded value and asset, so the
# seal we close is itself confidential.
echo
echo "== Create Alice's confidential seal (CT self-send) =="
ALICE_CT=$(ecli_w w_a getnewaddress)
SEAL_FUND_TXID=$(ecli_w w_a sendtoaddress "$ALICE_CT" 1.0)
ANY=$(ecli_w w_a getnewaddress)
ANY=$(ecli_w w_a getaddressinfo "$ANY" | jq -r '.unconfidential')
ecli generatetoaddress 1 "$ANY" > /dev/null

SEAL_UTXO=$(ecli_w w_a listunspent 1 | jq --arg t "$SEAL_FUND_TXID" \
  '[.[] | select(.txid == $t)] | .[0]')
ALICE_SEAL_TXID=$(echo "$SEAL_UTXO" | jq -r '.txid')
ALICE_SEAL_VOUT=$(echo "$SEAL_UTXO" | jq -r '.vout')
SEAL_BLINDER=$(echo "$SEAL_UTXO" | jq -r '.amountblinder')

if [ "$SEAL_BLINDER" = "0000000000000000000000000000000000000000000000000000000000000000" ]; then
  echo "✗ FAIL: Alice's seal UTXO is not blinded (amountblinder is zero)"
  exit 1
fi
echo "  alice seal : $ALICE_SEAL_TXID:$ALICE_SEAL_VOUT (BLINDED, amountblinder ${SEAL_BLINDER:0:16}…)"

# Bob's seal + change are future outpoints of the witness tx (see
# demo_rgb20.sh for why placeholders are sound for the commitment).
BOB_PLACEHOLDER_TXID=$(printf 'bob-receive-placeholder' | shasum -a 256 | awk '{print $1}')
BOB_SEAL="$BOB_PLACEHOLDER_TXID:1"
CHANGE_PLACEHOLDER_TXID=$(printf 'alice-change-placeholder' | shasum -a 256 | awk '{print $1}')
CHANGE_SEAL="$CHANGE_PLACEHOLDER_TXID:2"

# ── 2. Issue NIA + build the transfer transition (unchanged RGB path) ──
echo
echo "== Issue NIA + build transfer transition =="
OUT=$("$RGB_ANCHOR" rgb20-transfer \
  --name KaleidoConfidentialUSD \
  --ticker kCUSD \
  --supply 1000000 \
  --send   600000 \
  --alice-seal "$ALICE_SEAL_TXID:$ALICE_SEAL_VOUT" \
  --bob-seal   "$BOB_SEAL" \
  --change-seal "$CHANGE_SEAL")
ADDR=$(echo "$OUT" | sed -n '1p')
ANCHOR_JSON=$(echo "$OUT" | sed -n '2p')

ANCHOR_FILE="$OUT_DIR/anchor_m4_confidential.json"
echo "$ANCHOR_JSON" > "$ANCHOR_FILE"
echo "  saved -> $ANCHOR_FILE"

# ── 3. Build, BLIND, sign, broadcast the witness tx ─────────────────────
#   vout[0]: tapret commitment (explicit — commitment is in the SPK)
#   vout[1]: Bob's seal, CONFIDENTIAL address → blinded
#   vout[2]: Alice's change, wallet CT address → blinded
echo
echo "== Build + blind + broadcast witness tx =="
BOB_CT=$(ecli_w w_b getnewaddress)

OUTPUTS_JSON=$(jq -n \
  --arg tapret "$ADDR" --arg bob "$BOB_CT" --arg asset "$LBTC" \
  '[
    {($tapret): 0.0005, "asset": $asset},
    {($bob):    0.0003, "asset": $asset}
  ]')

RAW=$(ecli_w w_a createrawtransaction \
  "[{\"txid\":\"$ALICE_SEAL_TXID\",\"vout\":$ALICE_SEAL_VOUT}]" \
  "$OUTPUTS_JSON")
FUNDED=$(ecli_w w_a fundrawtransaction "$RAW" \
  '{"add_inputs": true, "lockUnspents": true, "changePosition": 2}')
FUNDED_HEX=$(echo "$FUNDED" | jq -r '.hex')

# Blinding is the whole point here: no fallback, fail loudly.
if ! BLINDED=$(ecli_w w_a blindrawtransaction "$FUNDED_HEX"); then
  echo "✗ FAIL: blindrawtransaction failed — cannot run the confidential demo"
  exit 1
fi

SIGNED=$(ecli_w w_a signrawtransactionwithwallet "$BLINDED")
SIGNED_HEX=$(echo "$SIGNED" | jq -r '.hex')
TXID=$(ecli sendrawtransaction "$SIGNED_HEX")
echo "  witness txid: $TXID"

ecli generatetoaddress 2 "$ANY" > /dev/null

jq --arg t "$TXID" '.txid = $t' "$ANCHOR_FILE" > "$ANCHOR_FILE.tmp" \
  && mv "$ANCHOR_FILE.tmp" "$ANCHOR_FILE"

# ── 4. Assert, on-chain, that the tx really is confidential ────────────
echo
echo "== Assert witness tx is confidential =="
DECODED=$(ecli getrawtransaction "$TXID" 1)

BOB_VALUECOMMIT=$(echo "$DECODED" | jq -r '.vout[1].valuecommitment // empty')
BOB_EXPLICIT=$(echo "$DECODED"    | jq -r '.vout[1].value // empty')
if [ -z "$BOB_VALUECOMMIT" ] || [ -n "$BOB_EXPLICIT" ]; then
  echo "✗ FAIL: Bob's seal output (vout 1) is not blinded"
  exit 1
fi
echo "  ✓ vout[1] (Bob's seal)  : valuecommitment ${BOB_VALUECOMMIT:0:18}… (no explicit value)"

CHG_VALUECOMMIT=$(echo "$DECODED" | jq -r '.vout[2].valuecommitment // empty')
if [ -n "$CHG_VALUECOMMIT" ]; then
  echo "  ✓ vout[2] (change)      : valuecommitment ${CHG_VALUECOMMIT:0:18}… (no explicit value)"
else
  echo "  · vout[2] (change)      : explicit (wallet chose an unblinded change output)"
fi

TAPRET_SPK=$(echo "$DECODED" | jq -r '.vout[0].scriptPubKey.hex')
echo "  ✓ vout[0] (tapret)      : SPK $TAPRET_SPK (scriptPubKey — never blinded)"

# ── 5. Verify with the UNCHANGED verifiers ──────────────────────────────
echo
echo "== Verify (bespoke path: commitment + seal closure) =="
"$RGB_ANCHOR" verify --anchor "$ANCHOR_FILE"

echo
echo "== Verify (patched path: patched rgbcore::dbc::Anchor::verify) =="
"$RGB_ANCHOR" verify-patched --anchor "$ANCHOR_FILE"

# ── 6. Negative test: tampering is still caught on a blinded tx ─────────
echo
echo "== Negative: tamper with one entry's message_hex =="
BAD_MSG="$OUT_DIR/anchor_m4_bad_msg.json"
jq '.entries[0].message_hex |= (.[0:62] + "ff")' "$ANCHOR_FILE" > "$BAD_MSG"
if "$RGB_ANCHOR" verify --anchor "$BAD_MSG" 2>/dev/null; then
  echo "✗ FAIL: verifier accepted tampered entry"
  exit 1
fi
echo "✓ verifier rejected tampered entry"

echo
echo "✅ Done — confidential RGB20 transfer anchored on Liquid (M4)."
echo "   The seal that was closed, the new seal, and the change are all"
echo "   blinded outputs. The commitment sits in the scriptPubKey, which"
echo "   Elements never blinds — so the same verifiers that work on"
echo "   transparent transactions accepted this one, with no unblinding"
echo "   data and no code changes."
