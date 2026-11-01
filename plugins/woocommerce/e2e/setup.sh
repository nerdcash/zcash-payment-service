#!/usr/bin/env bash
# Installs WordPress and WooCommerce in the e2e stack and configures the gateway.
set -euo pipefail
cd "$(dirname "$0")"
WOOCOMMERCE_VERSION="${WOOCOMMERCE_VERSION:-11.2.1}"
WP_URL="${E2E_WP_URL:-http://localhost:${E2E_WP_PORT:-8080}}"
# How WordPress (inside its container) reaches the service.
SERVICE_URL_FOR_WP="${E2E_SERVICE_URL_FOR_WP:-http://zcash-payment-service:8787}"
# Optional: a local woocommerce.zip to install instead of downloading from wordpress.org.
WOOCOMMERCE_ZIP="${WOOCOMMERCE_ZIP:-}"
wp() { docker compose run --rm -T ${WOOCOMMERCE_ZIP:+-v "$(realpath "$WOOCOMMERCE_ZIP"):/tmp/woocommerce.zip:ro"} wpcli wp "$@"; }

wp core install --url="$WP_URL" --title="Zcash e2e store" \
  --admin_user=admin --admin_password=admin --admin_email=admin@example.com --skip-email
if [ -n "$WOOCOMMERCE_ZIP" ]; then
  wp plugin install /tmp/woocommerce.zip --activate
else
  wp plugin install woocommerce --version="$WOOCOMMERCE_VERSION" --activate
fi
wp plugin activate zcash-payment-gateway
wp option update woocommerce_coming_soon no
wp option update woocommerce_currency ZEC
wp option update woocommerce_price_num_decimals 8
wp option update woocommerce_default_country US:CO
wp option update woocommerce_onboarding_profile '{"skipped":true}' --format=json
wp option update woocommerce_zcash_payment_settings --format=json \
  '{"enabled":"yes","title":"Zcash","description":"Pay with Zcash","service_url":"'"$SERVICE_URL_FOR_WP"'","webhook_secret":"e2e-webhook-secret","pay_base_url":"","zec_rate":""}'
# Make sure WooCommerce pages (cart, checkout) exist.
wp wc tool run install_pages --user=admin >/dev/null
wp wc product create --user=admin --name="Test mug" --type=simple --virtual=true \
  --regular_price=0.5 --porcelain
