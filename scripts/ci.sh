#!/usr/bin/env bash
#
# Full-flow CI: nuke -> bring up -> bootstrap -> test -> nuke.

set -euo pipefail

cd "$(dirname "$0")/.."

COMPOSE="${COMPOSE:-docker compose}"

cleanup() {
  echo "== Cleanup =="
  ./scripts/teardown.sh || true
}
trap cleanup EXIT

echo "== Pre-clean =="
./scripts/teardown.sh

echo "== Bring up stack =="
$COMPOSE up -d

echo "== Bootstrap =="
./scripts/bootstrap.sh

echo "== Build =="
cargo build --workspace --all-targets

echo "== Test =="
cargo test --workspace -- --nocapture

echo "✓ CI green."
