#!/usr/bin/env bash
#
# Bootstrap a fresh Liquid regtest chain for the demos.
#
# Liquid regtest has no block subsidy. The chain's `initialfreecoins`
# parameter pays the genesis output to OP_TRUE (`raw(51)`). Descriptor
# wallets can't easily spend OP_TRUE outputs, so we construct a raw tx
# directly to seed w_a and w_b with L-BTC, then use normal wallet RPCs
# from there.
#
# Flow:
#   1. Wait for elementsd RPC.
#   2. Create three descriptor wallets: w_issuer, w_a, w_b.
#   3. Locate the OP_TRUE UTXO with scantxoutset.
#   4. Build & broadcast a raw tx splitting it: 10 L-BTC to w_a, 10 to w_b,
#      remainder to fee. Empty scriptSig spends OP_TRUE.
#   5. Mine 2 blocks (need a dummy address — use w_a's).
#   6. Send 1 L-BTC from w_a → w_issuer so the issuer can pay issuance fees.
#   7. Issue 1000 units of a test asset; send 100 to w_a.
#   8. Write fixtures to out/.

source "$(dirname "$0")/_lib.sh"

wait_for_node

LBTC="b2e15d0d7a0c94e4e2ce0fe6e8691b9e451377f6e46e8045a86f7c4b5d4f0f23"

create_descriptor_wallet() {
  local w="$1"
  if ecli listwallets | grep -q "\"$w\""; then return; fi
  if ecli listwalletdir | grep -q "\"name\": \"$w\""; then
    ecli loadwallet "$w" > /dev/null
    return
  fi
  ecli createwallet "$w" false false "" false true true > /dev/null
}

echo "== Creating descriptor wallets =="
for w in w_issuer w_a w_b; do
  create_descriptor_wallet "$w"
  echo "  $w ready"
done

echo "== Locating OP_TRUE genesis UTXO =="
SCAN=$(ecli scantxoutset start '["raw(51)"]')
GEN_TXID=$(echo "$SCAN" | jq -r '.unspents[0].txid')
GEN_VOUT=$(echo "$SCAN" | jq -r '.unspents[0].vout')
GEN_AMOUNT=$(echo "$SCAN" | jq -r '.unspents[0].amount')
echo "  txid: $GEN_TXID"
echo "  amount: $GEN_AMOUNT L-BTC"

echo "== Getting funding addresses =="
A_CT=$(ecli_w w_a getnewaddress)
B_CT=$(ecli_w w_b getnewaddress)
ISSUER_CT=$(ecli_w w_issuer getnewaddress)
A_ADDR=$(ecli_w w_a getaddressinfo "$A_CT" | jq -r '.unconfidential')
B_ADDR=$(ecli_w w_b getaddressinfo "$B_CT" | jq -r '.unconfidential')
ISSUER_ADDR=$(ecli_w w_issuer getaddressinfo "$ISSUER_CT" | jq -r '.unconfidential')

echo "== Building OP_TRUE-spending tx (seeds w_a/w_b/w_issuer with 10 L-BTC each) =="
# Send 10 to each, burn the rest as fee. The amount is in BTC (float).
# 21,000,000 - 30 = 20,999,970 fee
OUTPUTS=$(jq -n \
  --arg a "$A_ADDR" --arg b "$B_ADDR" --arg i "$ISSUER_ADDR" --arg asset "$LBTC" \
  '[
    {($a): 10, "asset": $asset},
    {($b): 10, "asset": $asset},
    {($i): 10, "asset": $asset},
    {"fee": 20999970, "asset": $asset}
  ]')
RAW=$(ecli createrawtransaction \
  "[{\"txid\":\"$GEN_TXID\",\"vout\":$GEN_VOUT}]" \
  "$OUTPUTS")
SEED_TXID=$(ecli sendrawtransaction "$RAW" 0)
echo "  seed txid: $SEED_TXID"

echo "== Mining 2 confirmations =="
ecli generatetoaddress 2 "$A_ADDR" > /dev/null

echo "== Issuing test asset (1000 units) =="
ISSUE_JSON=$(ecli_w w_issuer issueasset 1000 0)
ASSET_ID=$(echo "$ISSUE_JSON" | jq -r '.asset')
ENTROPY=$(echo "$ISSUE_JSON" | jq -r '.entropy')
ecli generatetoaddress 2 "$A_ADDR" > /dev/null
echo "  asset_id = $ASSET_ID"

echo "== Sending 100 asset units to w_a =="
ecli_w w_issuer sendtoaddress \
  "$A_CT" 100 "" "" false false 1 "UNSET" false "$ASSET_ID" > /dev/null
ecli generatetoaddress 2 "$A_ADDR" > /dev/null

echo "== Writing fixtures =="
jq -n \
  --arg asset "$ASSET_ID" \
  --arg entropy "$ENTROPY" \
  --arg lbtc "$LBTC" \
  '{asset_id: $asset, entropy: $entropy, issued: 1000, units_to_a: 100, lbtc_asset: $lbtc}' \
  > "$OUT_DIR/asset.json"

jq -n \
  --arg a_ct "$A_CT" --arg a "$A_ADDR" \
  --arg b_ct "$B_CT" --arg b "$B_ADDR" \
  --arg i_ct "$ISSUER_CT" --arg i "$ISSUER_ADDR" \
  '{
    a:        {confidential: $a_ct, unconfidential: $a},
    b:        {confidential: $b_ct, unconfidential: $b},
    issuer:   {confidential: $i_ct, unconfidential: $i}
  }' > "$OUT_DIR/addresses.json"

echo
echo "✓ Bootstrap complete."
echo "  w_a       L-BTC: $(ecli_w w_a getbalance | jq -r '.bitcoin // 0')"
echo "  w_a       asset: $(ecli_w w_a getbalance | jq -r ".[\"$ASSET_ID\"] // 0")"
echo "  w_b       L-BTC: $(ecli_w w_b getbalance | jq -r '.bitcoin // 0')"
echo "  w_issuer  L-BTC: $(ecli_w w_issuer getbalance | jq -r '.bitcoin // 0')"
echo "  fixtures -> $OUT_DIR/"
