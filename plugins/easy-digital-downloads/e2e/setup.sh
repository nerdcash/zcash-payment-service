#!/usr/bin/env bash
# Installs WordPress and Easy Digital Downloads in the e2e stack and configures the gateway.
# Prints the test download's ID on the last line.
set -euo pipefail
cd "$(dirname "$0")"
EDD_VERSION="${EDD_VERSION:-3.7.3}"
WP_URL="${E2E_WP_URL:-http://localhost:${E2E_WP_PORT:-8080}}"
# How WordPress (inside its container) reaches the service.
SERVICE_URL_FOR_WP="${E2E_SERVICE_URL_FOR_WP:-http://zcash-payment-service:8787}"
# Optional: a local easy-digital-downloads.zip to install instead of downloading from wordpress.org.
EDD_ZIP="${EDD_ZIP:-}"
wp() { docker compose run --rm -T ${EDD_ZIP:+-v "$(realpath "$EDD_ZIP"):/tmp/edd.zip:ro"} wpcli wp "$@"; }

wp core install --url="$WP_URL" --title="Zcash e2e store" \
  --admin_user=admin --admin_password=admin --admin_email=admin@example.com --skip-email >&2
wp rewrite structure '/%postname%/' >&2
if [ -n "$EDD_ZIP" ]; then
  wp plugin install /tmp/edd.zip --activate >&2
else
  wp plugin install easy-digital-downloads --version="$EDD_VERSION" --activate >&2
fi
wp plugin activate zcash-payment-edd >&2
# Create the store pages if activation didn't, then point EDD at them and configure the gateway.
wp eval '
foreach ( array( "purchase_page" => array( "Checkout", "[download_checkout]" ), "success_page" => array( "Receipt", "[edd_receipt]" ), "failure_page" => array( "Transaction Failed", "Your transaction failed." ), "purchase_history_page" => array( "Order History", "[purchase_history]" ) ) as $key => $page ) {
	$id = (int) edd_get_option( $key );
	if ( ! $id || ! get_post( $id ) ) {
		$id = wp_insert_post( array( "post_title" => $page[0], "post_content" => $page[1], "post_status" => "publish", "post_type" => "page" ) );
		edd_update_option( $key, $id );
	}
}
edd_update_option( "currency", "ZEC" );
edd_update_option( "gateways", array( "zcash" => "1" ) );
edd_update_option( "default_gateway", "zcash" );
edd_update_option( "zcash_service_url", "'"$SERVICE_URL_FOR_WP"'" );
edd_update_option( "zcash_webhook_secret", "e2e-webhook-secret" );
edd_update_option( "zcash_pay_base_url", "" );
edd_update_option( "zcash_zec_rate", "" );
' >&2
download_id="$(wp post create --post_type=download --post_title="Test e-book" --post_status=publish --porcelain)"
wp post meta update "$download_id" edd_price 0.5 >&2
echo "$download_id"
