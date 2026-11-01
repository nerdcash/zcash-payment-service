#!/usr/bin/env bash
# Full PrestaShop end-to-end test. Needs Docker, Node.js, and internet access to pull images.
# No store, wallet, or payment accounts are involved.
#   KEEP_STACK=1 ./run.sh   leaves the stack running afterwards.
set -euo pipefail
cd "$(dirname "$0")"
export E2E_PS_URL="${E2E_PS_URL:-http://localhost:${E2E_PS_PORT:-8080}}"
export E2E_SERVICE_URL="${E2E_SERVICE_URL:-http://localhost:${E2E_SERVICE_PORT:-8787}}"
WEBHOOK_SECRET=e2e-webhook-secret
WEBHOOK_URL="$E2E_PS_URL/index.php?fc=module&module=zcashpayment&controller=webhook"
# Runs PHP from stdin inside the shop, after PrestaShop's bootstrap and loading the module's classes.
ps() { { echo '<?php require "/var/www/html/config/config.inc.php"; Module::getInstanceByName("zcashpayment");'; cat; } | docker compose exec -T prestashop php 2>/dev/null; }
notes() { echo "foreach (Message::getMessagesByOrderId($1, true) as \$m) echo \$m['message'], PHP_EOL;" | ps; }
row() { echo "echo ZcashPaymentOrder::get($1)['$2'];" | ps; }
state() { echo "echo (new Order($1))->current_state;" | ps; }

cleanup() {
  status=$?
  if [ $status -ne 0 ]; then
    docker compose logs --no-color --tail=200 zcash-payment-service prestashop || true
  fi
  if [ -z "${KEEP_STACK:-}" ]; then docker compose down -v >/dev/null 2>&1 || true; fi
  exit $status
}
trap cleanup EXIT

docker compose up -d ${E2E_UP_ARGS:---build} --wait
docker compose exec -T -u www-data prestashop php bin/console prestashop:module install zcashpayment
export E2E_PRODUCT_ID="$(docker compose exec -T prestashop php /e2e/setup.php "${E2E_SERVICE_URL_FOR_PS:-http://localhost:8787}" | tail -n 1)"
echo "product $E2E_PRODUCT_ID"

[ -d node_modules ] || npm ci --no-audit --no-fund
order_id="$(node checkout.mjs | tee /dev/stderr | sed -n 's/^ORDER_ID=//p')"

echo "--- order $order_id"
paid_state="$(echo 'echo Configuration::get("PS_OS_PAYMENT");' | ps)"
# The pay page follows the service's status; the order follows the webhook, which can lag.
for _ in $(seq 1 30); do
  order_state="$(state "$order_id")"
  [ "$order_state" = "$paid_state" ] && break
  sleep 2
done
echo "state: $order_state"
[ "$order_state" = "$paid_state" ] || { echo "order not paid"; exit 1; }
received="$(row "$order_id" received_zat)"
[ "$received" = "50000000" ] || { echo "unexpected received total $received"; exit 1; }
# Moving to a paid state records an order payment for the full amount.
paid_total="$(echo "\$o = new Order($order_id); echo (float) Db::getInstance()->getValue('SELECT SUM(amount) FROM ' . _DB_PREFIX_ . 'order_payment WHERE order_reference = \\'' . pSQL(\$o->reference) . '\\'');" | ps)"
echo "recorded payments: $paid_total"
[ "$paid_total" = "0.5" ] || { echo "unexpected recorded payments $paid_total"; exit 1; }
notes "$order_id"

echo "--- a late payment never revives a cancelled order"
late_id="$(E2E_STOP_AT_PAY_PAGE=1 node checkout.mjs | sed -n 's/^ORDER_ID=//p')"
cancelled="$(echo 'echo Configuration::get("PS_OS_CANCELED");' | ps)"
# From the CLI the state change lands but a later Symfony-only step throws, so check the state.
echo "ZcashPayment::set_state($late_id, $cancelled);" | ps || true
[ "$(state "$late_id")" = "$cancelled" ] || { echo "could not cancel order $late_id"; exit 1; }
late_address="$(row "$late_id" address)"
[ -n "$late_address" ] || { echo "late order has no address"; exit 1; }
curl -sf -X POST "$E2E_SERVICE_URL/dev/simulate-payment" -H 'content-type: application/json' \
  --data "{\"address\":\"$late_address\",\"amount\":\"0.5\"}" >/dev/null
for _ in $(seq 1 30); do
  late_notes="$(notes "$late_id")"
  case "$late_notes" in *"under manual review"*) break ;; esac
  sleep 2
done
# Cancelling an order with a Zcash address sets its funds aside for review, so the late payment
# is held there.
echo "$late_notes" | grep -q "under manual review" || { echo "no late-payment note: $late_notes"; exit 1; }
[ "$(row "$late_id" needs_review)" = 1 ] || { echo "late payment not flagged for review"; exit 1; }
[ "$(state "$late_id")" = "$cancelled" ] || { echo "cancelled order was revived"; exit 1; }

echo "--- an order is paid only once its payment has REQUIRED_CONFIRMATIONS"
conf_id="$(E2E_STOP_AT_PAY_PAGE=1 node checkout.mjs | sed -n 's/^ORDER_ID=//p')"
conf_address="$(row "$conf_id" address)"
[ -n "$conf_address" ] || { echo "confirmation order has no address"; exit 1; }
awaiting="$(state "$conf_id")"
txid="$(printf 'cd%.0s' $(seq 1 32))"
simulate_depth() {
  curl -sf -X POST "$E2E_SERVICE_URL/dev/simulate-payment" -H 'content-type: application/json' \
    --data "{\"address\":\"$conf_address\",\"amount\":\"0.5\",\"txid\":\"$txid\",\"confirmations\":$1}" >/dev/null
}
simulate_depth 0
for _ in $(seq 1 30); do
  conf_notes="$(notes "$conf_id")"
  case "$conf_notes" in *"pending confirmation"*) break ;; esac
  sleep 2
done
echo "$conf_notes" | grep -q "pending confirmation" || { echo "no pending-confirmation note: $conf_notes"; exit 1; }
[ "$(state "$conf_id")" = "$awaiting" ] || { echo "order paid from the mempool"; exit 1; }
[ "$(row "$conf_id" seen_zat)" = 50000000 ] || { echo "seen amount not recorded"; exit 1; }
simulate_depth 1
sleep 6
[ "$(state "$conf_id")" = "$awaiting" ] || { echo "order paid at 1 of 2 confirmations"; exit 1; }
[ "$(notes "$conf_id" | grep -c "pending confirmation")" = 1 ] || { echo "pending note repeated"; exit 1; }
simulate_depth 2
for _ in $(seq 1 30); do
  conf_state="$(state "$conf_id")"
  [ "$conf_state" = "$paid_state" ] && break
  sleep 2
done
[ "$conf_state" = "$paid_state" ] || { echo "confirmed order not paid: $conf_state"; exit 1; }
[ "$(row "$conf_id" received_zat)" = 50000000 ] || { echo "settled amount not recorded"; exit 1; }

echo "--- webhook rejects a bad signature"
body='{"event_id":"zcash:1:v9","payment_source":"zcash","address":"x","total_received":"9","observed_at":"2026-01-01T00:00:00Z"}'
code="$(curl -s -o /dev/null -w '%{http_code}' -X POST "$WEBHOOK_URL" \
  -H 'content-type: application/json' -H 'X-Signature: 00' --data "$body")"
[ "$code" = 401 ] || { echo "bad signature returned $code"; exit 1; }

echo "--- signed webhook for an unknown address is acknowledged and ignored"
sig="$(printf '%s' "$body" | openssl dgst -sha256 -hmac "$WEBHOOK_SECRET" -r | cut -d' ' -f1)"
code="$(curl -s -o /dev/null -w '%{http_code}' -X POST "$WEBHOOK_URL" \
  -H 'content-type: application/json' -H "X-Signature: $sig" --data "$body")"
[ "$code" = 200 ] || { echo "unknown address returned $code"; exit 1; }

echo "e2e passed"
