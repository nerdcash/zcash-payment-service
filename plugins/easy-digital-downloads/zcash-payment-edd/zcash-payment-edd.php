<?php
/**
 * Plugin Name:       Zcash Payments for Easy Digital Downloads (self-hosted)
 * Plugin URI:        https://github.com/nerdcash/zcash-payment-service
 * Description:       Accept shielded Zcash payments in Easy Digital Downloads through your own zcash-payment-service. No middleman, no fees.
 * Version:           0.1.0
 * Requires at least: 6.4
 * Requires PHP:      8.1
 * Requires Plugins:  easy-digital-downloads
 * Author:            nerdcash
 * License:           MIT
 * License URI:       https://opensource.org/licenses/MIT
 * Text Domain:       zcash-payment-edd
 */

defined( 'ABSPATH' ) || exit;

define( 'ZCASH_EDD_VERSION', '0.1.0' );
define( 'ZCASH_EDD_DIR', plugin_dir_path( __FILE__ ) );
// The order tables, receipt hashes, and on-hold status this plugin uses.
define( 'ZCASH_EDD_MIN_EDD', '3.1' );

require_once ZCASH_EDD_DIR . 'includes/class-zcash-edd-util.php';

// ZEC as a store currency, so prices can be set directly in ZEC, with 8 decimal places.
add_filter(
	'edd_currencies',
	static function ( $currencies ) {
		$currencies['ZEC'] = __( 'Zcash (ZEC)', 'zcash-payment-edd' );
		return $currencies;
	}
);
add_filter(
	'edd_currency_decimal_count',
	static function ( $decimals, $currency = '' ) {
		return 'ZEC' === strtoupper( (string) $currency ) ? 8 : $decimals;
	},
	10,
	2
);
add_filter(
	'edd_currency_symbol',
	static function ( $symbol, $currency = '' ) {
		return 'ZEC' === strtoupper( (string) $currency ) ? 'ZEC' : $symbol;
	},
	10,
	2
);
add_filter(
	'edd_format_amount_decimals',
	static function ( $decimals, $amount = 0, $currency = '' ) {
		return 'ZEC' === strtoupper( (string) $currency ) && $decimals > 0 ? 8 : $decimals;
	},
	10,
	3
);

add_action(
	'plugins_loaded',
	static function () {
		if ( ! function_exists( 'edd_get_order' ) || ! defined( 'EDD_VERSION' ) ) {
			return;
		}
		if ( version_compare( EDD_VERSION, ZCASH_EDD_MIN_EDD, '<' ) ) {
			add_action(
				'admin_notices',
				static function () {
					echo '<div class="notice notice-error"><p>' . esc_html(
						sprintf(
							/* translators: %s: minimum Easy Digital Downloads version */
							__( 'Zcash Payments for Easy Digital Downloads needs Easy Digital Downloads %s or newer and is inactive.', 'zcash-payment-edd' ),
							ZCASH_EDD_MIN_EDD
						)
					) . '</p></div>';
				}
			);
			return;
		}
		require_once ZCASH_EDD_DIR . 'includes/class-zcash-edd-gateway.php';
		require_once ZCASH_EDD_DIR . 'includes/class-zcash-edd-webhook.php';
		Zcash_EDD_Gateway::register();
		Zcash_EDD_Webhook::register();
	}
);
