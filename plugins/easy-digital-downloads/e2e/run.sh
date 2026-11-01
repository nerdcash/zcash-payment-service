#!/usr/bin/env bash
# Full Easy Digital Downloads end-to-end test. Needs Docker, Node.js, and internet access to pull
# images and download EDD. No store, wallet, or payment accounts are involved.
#   KEEP_STACK=1 ./run.sh   leaves the stack running afterwards.
set -euo pipefail
cd "$(dirname "$0")"
export E2E_WP_URL="${E2E_WP_URL:-http://localhost:${E2E_WP_PORT:-8080}}"
export E2E_SERVICE_URL="${E2E_SERVICE_URL:-http://localhost:${E2E_SERVICE_PORT:-8787}}"
WEBHOOK_SECRET=e2e-webhook-secret
WEBHOOK_URL="$E2E_WP_URL/?rest_route=/zcash-payment-edd/v1/webhook"
wp() { docker compose run --rm -T wpcli wp "$@" 2>/dev/null; }
notes() { wp eval "foreach (edd_get_notes(['object_id' => $1, 'object_type' => 'order']) as \$n) echo \$n->content, PHP_EOL;"; }

cleanup() {
  status=$?
  if [ $status -ne 0 ]; then
    docker compose logs --no-color --tail=200 zcash-payment-service wordpress || true
  fi
  if [ -z "${KEEP_STACK:-}" ]; then docker compose down -v >/dev/null 2>&1 || true; fi
  exit $status
}
trap cleanup EXIT

docker compose up -d ${E2E_UP_ARGS:---build} --wait
download_id="$(./setup.sh | tail -n 1)"
export E2E_DOWNLOAD_ID="$download_id"
export E2E_CHECKOUT_URL="$(wp eval 'echo edd_get_checkout_uri();')"
echo "download $E2E_DOWNLOAD_ID, checkout $E2E_CHECKOUT_URL"

[ -d node_modules ] || npm ci --no-audit --no-fund
order_id="$(node checkout.mjs | tee /dev/stderr | sed -n 's/^ORDER_ID=//p')"

echo "--- order $order_id"
# The pay page follows the service's status; the order follows the webhook, which can lag.
for _ in $(seq 1 30); do
  order_status="$(wp eval "echo edd_get_order($order_id)->status;")"
  [ "$order_status" = complete ] && break
  sleep 2
done
echo "status: $order_status"
[ "$order_status" = complete ] || { echo "order not paid"; exit 1; }
received="$(wp eval "echo edd_get_order_meta($order_id, '_zcash_received_zat', true);")"
[ "$received" = "50000000" ] || { echo "unexpected received total $received"; exit 1; }
notes "$order_id"

echo "--- a late payment never revives a revoked order"
late_id="$(wp eval '
$id = edd_add_order( array( "status" => "pending", "gateway" => "zcash", "currency" => "ZEC", "email" => "late@example.com", "subtotal" => 0.5, "total" => 0.5 ) );
edd_add_order_item( array( "order_id" => $id, "product_id" => '"$E2E_DOWNLOAD_ID"', "product_name" => "Test e-book", "status" => "pending", "amount" => 0.5, "subtotal" => 0.5, "total" => 0.5, "quantity" => 1 ) );
$r = Zcash_EDD_Gateway::start_session( $id );
if ( is_wp_error( $r ) ) { fwrite( STDERR, $r->get_error_message() ); exit( 1 ); }
edd_update_order_status( $id, "revoked" );
echo $id;
')"
late_address="$(wp eval "echo edd_get_order_meta($late_id, '_zcash_address', true);")"
[ -n "$late_address" ] || { echo "late order has no address"; exit 1; }
curl -sf -X POST "$E2E_SERVICE_URL/dev/simulate-payment" -H 'content-type: application/json' \
  --data "{\"address\":\"$late_address\",\"amount\":\"0.5\"}" >/dev/null
for _ in $(seq 1 30); do
  late_notes="$(notes "$late_id")"
  case "$late_notes" in *"under manual review"*) break ;; esac
  sleep 2
done
# Revoking an order with a Zcash address sets its funds aside for review, so the late payment
# is held there.
echo "$late_notes" | grep -q "under manual review" || { echo "no late-payment note: $late_notes"; exit 1; }
[ "$(wp eval "echo edd_get_order_meta($late_id, '_zcash_needs_review', true);")" = 1 ] || { echo "late payment not flagged for review"; exit 1; }
[ "$(wp eval "echo edd_get_order($late_id)->status;")" = revoked ] || { echo "revoked order was revived"; exit 1; }

echo "--- an order completes only once its payment has REQUIRED_CONFIRMATIONS"
conf_id="$(wp eval '
$id = edd_add_order( array( "status" => "pending", "gateway" => "zcash", "currency" => "ZEC", "email" => "conf@example.com", "subtotal" => 0.5, "total" => 0.5 ) );
edd_add_order_item( array( "order_id" => $id, "product_id" => '"$E2E_DOWNLOAD_ID"', "product_name" => "Test e-book", "status" => "pending", "amount" => 0.5, "subtotal" => 0.5, "total" => 0.5, "quantity" => 1 ) );
$r = Zcash_EDD_Gateway::start_session( $id );
if ( is_wp_error( $r ) ) { fwrite( STDERR, $r->get_error_message() ); exit( 1 ); }
echo $id;
')"
conf_address="$(wp eval "echo edd_get_order_meta($conf_id, '_zcash_address', true);")"
[ -n "$conf_address" ] || { echo "confirmation order has no address"; exit 1; }
txid="$(printf '12%.0s' $(seq 1 32))"
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
[ "$(wp eval "echo edd_get_order($conf_id)->status;")" = pending ] || { echo "order completed from the mempool"; exit 1; }
simulate_depth 1
sleep 6
[ "$(wp eval "echo edd_get_order($conf_id)->status;")" = pending ] || { echo "order completed at 1 of 2 confirmations"; exit 1; }
simulate_depth 2
for _ in $(seq 1 30); do
  conf_status="$(wp eval "echo edd_get_order($conf_id)->status;")"
  [ "$conf_status" = complete ] && break
  sleep 2
done
[ "$conf_status" = complete ] || { echo "confirmed order not complete: $conf_status"; exit 1; }

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
