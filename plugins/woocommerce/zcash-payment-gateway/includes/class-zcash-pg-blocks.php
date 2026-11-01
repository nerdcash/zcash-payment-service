<?php
/**
 * Makes the gateway selectable in the WooCommerce checkout block.
 */

defined( 'ABSPATH' ) || exit;

use Automattic\WooCommerce\Blocks\Payments\Integrations\AbstractPaymentMethodType;

final class Zcash_PG_Blocks extends AbstractPaymentMethodType {
	protected $name = 'zcash_payment';

	public function initialize() {
		$this->settings = get_option( 'woocommerce_zcash_payment_settings', array() );
	}

	public function is_active() {
		$gateways = WC()->payment_gateways() ? WC()->payment_gateways()->payment_gateways() : array();
		return isset( $gateways[ $this->name ] ) && $gateways[ $this->name ]->is_available();
	}

	public function get_payment_method_script_handles() {
		wp_register_script(
			'zcash-payment-gateway-blocks',
			plugins_url( 'assets/blocks.js', ZCASH_PG_FILE ),
			array( 'wc-blocks-registry', 'wc-settings', 'wp-element', 'wp-html-entities' ),
			ZCASH_PG_VERSION,
			true
		);
		return array( 'zcash-payment-gateway-blocks' );
	}

	public function get_payment_method_data() {
		return array(
			'title'       => $this->get_setting( 'title', 'Zcash (ZEC)' ),
			'description' => $this->get_setting( 'description', '' ),
			'supports'    => array( 'products' ),
		);
	}
}
