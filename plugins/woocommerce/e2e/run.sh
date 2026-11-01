#!/usr/bin/env bash
# Full WooCommerce end-to-end test. Needs Docker, Node.js, and internet access to pull images and
# download WooCommerce. No store, wallet, or payment accounts are involved.
#   KEEP_STACK=1 ./run.sh   leaves the stack running afterwards.
set -euo pipefail
cd "$(dirname "$0")"
export E2E_WP_URL="${E2E_WP_URL:-http://localhost:${E2E_WP_PORT:-8080}}"
export E2E_SERVICE_URL="${E2E_SERVICE_URL:-http://localhost:${E2E_SERVICE_PORT:-8787}}"
WEBHOOK_SECRET=e2e-webhook-secret
wp() { docker compose run --rm -T wpcli wp "$@" 2>/dev/null; }

cleanup() {
  status=$?
  if [ $status -ne 0 ]; then
    docker compose logs --no-color --tail=200 zcash-payment-service wordpress || true
  fi
  if [ -z "${KEEP_STACK:-}" ]; then docker compose down -v >/dev/null 2>&1 || true; fi
  exit $status
}
trap cleanup EXIT

docker compose up -d --build --wait
product_id="$(./setup.sh | tail -n 1)"
export E2E_PRODUCT_ID="$product_id"
export E2E_CHECKOUT_URL="$(wp eval 'echo wc_get_checkout_url();')"
echo "product $E2E_PRODUCT_ID, checkout $E2E_CHECKOUT_URL"

[ -d node_modules ] || npm ci --no-audit --no-fund
order_id="$(node checkout.mjs | tee /dev/stderr | sed -n 's/^ORDER_ID=//p')"

echo "--- order $order_id"
# The pay page follows the service's status; the order follows the webhook, which can lag.
for _ in $(seq 1 30); do
  order_status="$(wp eval "echo wc_get_order($order_id)->get_status();")"
  case "$order_status" in processing|completed) break ;; esac
  sleep 2
done
echo "status: $order_status"
case "$order_status" in processing|completed) ;; *) echo "order not paid"; exit 1 ;; esac
received="$(wp eval "echo wc_get_order($order_id)->get_meta('_zcash_received_zat');")"
[ "$received" = "50000000" ] || { echo "unexpected received total $received"; exit 1; }
wp eval "foreach (wc_get_order_notes(['order_id' => $order_id]) as \$n) echo \$n->content, PHP_EOL;"

echo "--- a late payment never revives a cancelled order"
late_id="$(wp eval '
$o = wc_create_order();
$o->add_product( wc_get_product( '"$E2E_PRODUCT_ID"' ), 1 );
$o->set_payment_method( "zcash_payment" );
$o->calculate_totals();
$o->save();
$r = WC()->payment_gateways()->payment_gateways()["zcash_payment"]->process_payment( $o->get_id() );
if ( "success" !== $r["result"] ) { exit( 1 ); }
wc_get_order( $o->get_id() )->update_status( "cancelled" );
echo $o->get_id();
')"
late_address="$(wp eval "echo wc_get_order($late_id)->get_meta('_zcash_address');")"
curl -sf -X POST "$E2E_SERVICE_URL/dev/simulate-payment" -H 'content-type: application/json' \
  --data "{\"address\":\"$late_address\",\"amount\":\"0.5\"}" >/dev/null
for _ in $(seq 1 30); do
  notes="$(wp eval "foreach (wc_get_order_notes(['order_id' => $late_id]) as \$n) echo \$n->content, PHP_EOL;")"
  case "$notes" in *"after it was cancelled"*) break ;; esac
  sleep 2
done
echo "$notes" | grep -q "after it was cancelled" || { echo "no late-payment note: $notes"; exit 1; }
[ "$(wp eval "echo wc_get_order($late_id)->get_status();")" = cancelled ] || { echo "cancelled order was revived"; exit 1; }

echo "--- webhook rejects a bad signature"
body='{"event_id":"zcash:1:v9","payment_source":"zcash","address":"x","total_received":"9","observed_at":"2026-01-01T00:00:00Z"}'
code="$(curl -s -o /dev/null -w '%{http_code}' -X POST "$E2E_WP_URL/?rest_route=/zcash-payment/v1/webhook" \
  -H 'content-type: application/json' -H 'X-Signature: 00' --data "$body")"
[ "$code" = 401 ] || { echo "bad signature returned $code"; exit 1; }

echo "--- signed webhook for an unknown address is acknowledged and ignored"
sig="$(printf '%s' "$body" | openssl dgst -sha256 -hmac "$WEBHOOK_SECRET" -r | cut -d' ' -f1)"
code="$(curl -s -o /dev/null -w '%{http_code}' -X POST "$E2E_WP_URL/?rest_route=/zcash-payment/v1/webhook" \
  -H 'content-type: application/json' -H "X-Signature: $sig" --data "$body")"
[ "$code" = 200 ] || { echo "unknown address returned $code"; exit 1; }

echo "--- the service refuses a return_url outside RETURN_URL_ALLOWED_ORIGINS"
code="$(curl -s -o /dev/null -w '%{http_code}' -X POST "$E2E_SERVICE_URL/payment-session" \
  -H 'content-type: application/json' --data '{"payment_source":"zcash","amount":"0.1","return_url":"https://evil.example/"}')"
[ "$code" = 400 ] || { echo "foreign return_url returned $code"; exit 1; }

echo "e2e passed"
