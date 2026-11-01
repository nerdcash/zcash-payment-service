<?php
/**
 * Places a Zcash order from the cart and sends the customer to the pay page.
 */

if ( ! defined( '_PS_VERSION_' ) ) {
	exit;
}

class ZcashpaymentValidationModuleFrontController extends ModuleFrontController {
	public function postProcess() {
		$module = $this->module;
		$cart   = $this->context->cart;
		if ( ! $module instanceof ZcashPayment || ! $module->active || ! $cart->id || ! $cart->id_customer || ! $cart->id_address_delivery || ! $cart->id_address_invoice ) {
			Tools::redirect( 'index.php?controller=order&step=1' );
			return;
		}
		// The option must still be offered, in case the cart changed just before checkout.
		$offered = false;
		foreach ( Module::getPaymentModules() as $row ) {
			$offered = $offered || 'zcashpayment' === $row['name'];
		}
		$customer = new Customer( (int) $cart->id_customer );
		$currency = new Currency( (int) $cart->id_currency );
		if ( ! $offered || ! Validate::isLoadedObject( $customer ) || ! ZcashPayment::available_for( (string) $currency->iso_code ) ) {
			Tools::redirect( 'index.php?controller=order&step=1' );
			return;
		}

		// One Zcash address pays one order, so a cart PrestaShop would split is refused.
		if ( ! ZcashPayment::single_order( $cart ) ) {
			$this->errors[] = $this->module->l( 'Zcash can pay for orders shipped in a single package only.', 'validation' );
			$this->redirectWithNotifications( 'index.php?controller=order&step=1' );
			return;
		}

		// Check the amount converts before recording an order.
		$total = (float) $cart->getOrderTotal( true, Cart::BOTH );
		if ( null === ZcashPaymentUtil::order_total_to_zat( sprintf( '%.6F', $total ), (string) $currency->iso_code, ZcashPayment::option( ZcashPayment::CONFIG_ZEC_RATE ) ) ) {
			$this->errors[] = $this->module->l( 'This order total cannot be converted to ZEC.', 'validation' );
			$this->redirectWithNotifications( 'index.php?controller=order&step=1' );
			return;
		}

		$module->validateOrder( (int) $cart->id, ZcashPayment::awaiting_state(), $total, $module->displayName, null, array(), (int) $currency->id, false, $customer->secure_key );
		$id_order = (int) $module->currentOrder;
		$result   = $module->start_session( $id_order );
		if ( isset( $result['url'] ) ) {
			Tools::redirect( $result['url'] );
			return;
		}

		// Fail the order only if it is still an open Zcash order with no address, so nothing
		// can be paid to it; a state someone else set meanwhile is kept.
		$order = ZcashPayment::fresh_order( $id_order );
		try {
			if ( $order && ZcashPayment::is_ours( $order ) && ZcashPayment::is_open( (int) $order->current_state )
				&& '' === ZcashPaymentOrder::get( $id_order )['address'] ) {
				ZcashPayment::set_state( $id_order, (int) Configuration::get( 'PS_OS_ERROR' ) );
			}
		} catch ( ZcashPaymentDbException $e ) {
			// Unknown whether an address was saved, so leave the order open for the merchant.
			PrestaShopLogger::addLog( 'Zcash: ' . $e->getMessage(), 3, null, 'Order', $id_order );
		}
		$this->errors[] = (string) ( $result['error'] ?? '' );
		$this->redirectWithNotifications( $order ? $module->confirmation_url( $order ) : 'index.php?controller=order&step=1' );
	}
}
