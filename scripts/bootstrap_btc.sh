#!/usr/bin/env bash
#
# Bootstrap Bitcoin Core regtest for the cross-chain atomic swap.
#
# Creates a descriptor wallet `w_btc`, mines 101 blocks to it (so the
# first coinbase matures), leaving a spendable balance.

source "$(dirname "$0")/_lib.sh"

wait_for_bitcoind

echo "== Creating Bitcoin wallet w_btc =="
if bcli listwallets | grep -q '"w_btc"'; then
  echo "  already loaded"
elif bcli listwalletdir | grep -q '"name": "w_btc"'; then
  bcli loadwallet w_btc > /dev/null
  echo "  loaded from disk"
else
  bcli createwallet w_btc > /dev/null
  echo "  created"
fi

echo "== Mining 101 blocks =="
ADDR=$(bcli_w w_btc getnewaddress)
bcli_w w_btc generatetoaddress 101 "$ADDR" > /dev/null
echo "  mined to $ADDR"

BAL=$(bcli_w w_btc getbalance)
echo
echo "✓ Bitcoin regtest ready."
echo "  w_btc balance: $BAL BTC"
