#!/usr/bin/env bash
#
# Tear down the local stack and wipe state.

set -euo pipefail

cd "$(dirname "$0")/.."

COMPOSE="${COMPOSE:-docker compose}"

echo "== Stopping containers + removing volumes =="
$COMPOSE down -v --remove-orphans || true

echo "== Clearing out/ =="
find out -mindepth 1 ! -name '.gitkeep' -exec rm -rf {} + 2>/dev/null || true

echo "✓ Teardown complete."
