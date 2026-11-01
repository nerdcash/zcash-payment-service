<?php
/**
 * Plugin Name:       Zcash Payment Gateway (self-hosted)
 * Plugin URI:        https://github.com/nerdcash/zcash-payment-service
 * Description:       Accept shielded Zcash payments through your own zcash-payment-service. No middleman, no fees.
 * Version:           0.1.0
 * Requires at least: 6.4
 * Requires PHP:      8.1
 * Requires Plugins:  woocommerce
 * WC requires at least: 8.0
 * Author:            nerdcash
 * License:           MIT
 * License URI:       https://opensource.org/licenses/MIT
 * Text Domain:       zcash-payment-gateway
 */

defined( 'ABSPATH' ) || exit;

define( 'ZCASH_PG_VERSION', '0.1.0' );
define( 'ZCASH_PG_FILE', __FILE__ );
define( 'ZCASH_PG_DIR', plugin_dir_path( __FILE__ ) );

require_once ZCASH_PG_DIR . 'includes/class-zcash-pg-util.php';

// Declare compatibility with WooCommerce's order tables and the checkout block.
add_action(
	'before_woocommerce_init',
	static function () {
		if ( class_exists( \Automattic\WooCommerce\Utilities\FeaturesUtil::class ) ) {
			\Automattic\WooCommerce\Utilities\FeaturesUtil::declare_compatibility( 'custom_order_tables', __FILE__, true );
			\Automattic\WooCommerce\Utilities\FeaturesUtil::declare_compatibility( 'cart_checkout_blocks', __FILE__, true );
		}
	}
);

// ZEC as a store currency, so prices can be set directly in ZEC.
add_filter(
	'woocommerce_currencies',
	static function ( $currencies ) {
		$currencies['ZEC'] = __( 'Zcash', 'zcash-payment-gateway' );
		return $currencies;
	}
);
add_filter(
	'woocommerce_currency_symbol',
	static function ( $symbol, $currency ) {
		return 'ZEC' === $currency ? 'ZEC' : $symbol;
	},
	10,
	2
);

add_action(
	'plugins_loaded',
	static function () {
		if ( ! class_exists( 'WC_Payment_Gateway' ) ) {
			return;
		}
		require_once ZCASH_PG_DIR . 'includes/class-zcash-pg-gateway.php';
		require_once ZCASH_PG_DIR . 'includes/class-zcash-pg-webhook.php';

		add_filter(
			'woocommerce_payment_gateways',
			static function ( $gateways ) {
				$gateways[] = 'Zcash_PG_Gateway';
				return $gateways;
			}
		);

		Zcash_PG_Webhook::register();
	}
);

add_action(
	'woocommerce_blocks_loaded',
	static function () {
		if ( ! class_exists( \Automattic\WooCommerce\Blocks\Payments\Integrations\AbstractPaymentMethodType::class ) ) {
			return;
		}
		require_once ZCASH_PG_DIR . 'includes/class-zcash-pg-blocks.php';
		add_action(
			'woocommerce_blocks_payment_method_type_registration',
			static function ( $registry ) {
				$registry->register( new Zcash_PG_Blocks() );
			}
		);
	}
);
