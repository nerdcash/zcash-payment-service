#!/usr/bin/env bash
# Brings up deploy/compose.yml as a stranger to the internet would see it, on localhost with
# Caddy's internal CA, and checks what is public and what isn't. Needs Docker and curl.
#   KEEP_STACK=1 ./test.sh   leaves the stack running afterwards.
set -euo pipefail
cd "$(dirname "$0")/.."
export COMPOSE_PROJECT_NAME="${COMPOSE_PROJECT_NAME:-zcash-payment-deploy-test}"
# EXTRA_ENV_FILE can override test.env, e.g. to move ports.
dc() { docker compose --env-file test/test.env ${EXTRA_ENV_FILE:+--env-file "$EXTRA_ENV_FILE"} "$@"; }
set -a; . test/test.env; [ -z "${EXTRA_ENV_FILE:-}" ] || . "$EXTRA_ENV_FILE"; set +a
private="http://127.0.0.1:$PRIVATE_API_PORT"
public="https://localhost:$CADDY_HTTPS_PORT"
work="$(mktemp -d)"

cleanup() {
  status=$?
  if [ "$status" -ne 0 ]; then dc logs --no-color --tail 100 || true; fi
  if [ -z "${KEEP_STACK:-}" ]; then dc down -v >/dev/null 2>&1 || true; fi
  rm -rf "$work"
}
trap cleanup EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }
code() { curl -s -o /dev/null -w '%{http_code}' --cacert "$work/root.crt" "$@"; }

echo "--- the private API is published on loopback only"
# Checked on compose.yml itself, whatever overrides the test runs with.
docker compose -f compose.yml --env-file test/test.env config --format json | grep -q '"host_ip": "127.0.0.1"' || fail "private API port is not bound to 127.0.0.1"

dc up -d --wait ${E2E_UP_ARGS:-}
dc exec -T caddy cat /data/caddy/pki/authorities/local/root.crt > "$work/root.crt"

echo "--- a store creates a payment session over the private API"
session="$(curl -sf -X POST "$private/payment-session" -H 'content-type: application/json' \
  -d '{"payment_source":"zcash","amount":"0.1","label":"Deploy test","return_url":"https://shop.example/thanks"}')"
id="$(echo "$session" | sed -n 's/.*"payment_id":"\([^"]*\)".*/\1/p')"
address="$(echo "$session" | sed -n 's/.*"address":"\([^"]*\)".*/\1/p')"
pay_url="$(echo "$session" | sed -n 's/.*"pay_url":"\([^"]*\)".*/\1/p')"
[ -n "$id" ] && [ -n "$address" ] || fail "no session: $session"
[ "$pay_url" = "$public/pay/$id" ] || fail "pay_url is $pay_url"

echo "--- the pay page and status are served over HTTPS"
curl -sf --cacert "$work/root.crt" "$pay_url" | grep -q "$address" || fail "pay page"
curl -sf --cacert "$work/root.crt" "$public/payments/$id" | grep -q '"awaiting_payment"' || fail "status"
headers="$(curl -sfI --cacert "$work/root.crt" "$pay_url")"
echo "$headers" | grep -qi '^strict-transport-security' || fail "no HSTS"
echo "$headers" | grep -qi '^content-security-policy' || fail "no CSP"

echo "--- HTTP redirects to HTTPS"
# In production the ports are 80 and 443; Caddy's redirect leaves the port out.
location="$(curl -s -o /dev/null -w '%{redirect_url}' "http://localhost:$CADDY_HTTP_PORT/pay/$id")"
case "$location" in https://localhost*/pay/"$id") ;; *) fail "no redirect to HTTPS: $location" ;; esac

echo "--- nothing else is public"
for path in /payment-session /dev/simulate-payment; do
  [ "$(code -X POST -H 'content-type: application/json' -d '{}' "$public$path")" = 404 ] || fail "POST $path is public"
done
for path in /health / /pay /payments; do
  [ "$(code "$public$path")" = 404 ] || fail "GET $path is public"
done

echo "--- a payment shows on the public status route"
curl -sf -X POST "$private/dev/simulate-payment" -H 'content-type: application/json' \
  -d "{\"address\":\"$address\",\"amount\":\"0.1\"}" >/dev/null || fail "simulate-payment"
for _ in $(seq 20); do
  curl -sf --cacert "$work/root.crt" "$public/payments/$id" | grep -Eq '"(paid|received)"' && paid=1 && break
  sleep 1
done
[ -n "${paid:-}" ] || fail "payment not shown as paid"

echo "--- data and certificates survive a restart"
dc down
dc up -d --wait ${E2E_UP_ARGS:-}
curl -sf --cacert "$work/root.crt" "$public/payments/$id" | grep -Eq '"(paid|received)"' || fail "payment lost on restart"

echo "deploy test passed"
