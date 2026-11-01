#!/usr/bin/env bash
# Full Vendure end-to-end test. Needs Docker, Node.js, and internet access to pull images and npm
# packages. No store, wallet, or payment accounts are involved.
#   KEEP_STACK=1 ./run.sh   leaves the stack running afterwards (store on http://localhost:3000).
set -euo pipefail
cd "$(dirname "$0")"
export E2E_STORE_URL="${E2E_STORE_URL:-http://localhost:${E2E_VENDURE_PORT:-3000}}"
export E2E_SERVICE_URL="${E2E_SERVICE_URL:-http://localhost:${E2E_SERVICE_PORT:-8787}}"

cleanup() {
  status=$?
  if [ $status -ne 0 ]; then
    docker compose logs --no-color --tail=200 zcash-payment-service vendure || true
  fi
  if [ -z "${KEEP_STACK:-}" ]; then docker compose down -v >/dev/null 2>&1 || true; fi
  exit $status
}
trap cleanup EXIT

# E2E_UP_ARGS=--no-build reuses images that are already built or pulled.
docker compose up -d --wait ${E2E_UP_ARGS:---build}

[ -d node_modules ] || npm ci --no-audit --no-fund
node e2e.mjs
