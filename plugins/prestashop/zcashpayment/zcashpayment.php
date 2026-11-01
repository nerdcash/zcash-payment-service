<?php
/**
 * Zcash payments for PrestaShop through a self-hosted zcash-payment-service.
 *
 * @license MIT
 */

use PrestaShop\PrestaShop\Core\Payment\PaymentOption;

if ( ! defined( '_PS_VERSION_' ) ) {
	exit;
}

// The classes need PHP 8.1. This file stays loadable on older PHP so the module list can show
// why it won't install.
if ( PHP_VERSION_ID >= 80100 ) {
	require_once __DIR__ . '/classes/ZcashPaymentUtil.php';
	require_once __DIR__ . '/classes/ZcashPaymentBalance.php';
	require_once __DIR__ . '/classes/ZcashPaymentOrder.php';
	require_once __DIR__ . '/classes/ZcashPaymentWebhook.php';
}

class ZcashPayment extends PaymentModule {
	const CONFIG_SERVICE_URL    = 'ZCASH_SERVICE_URL';
	const CONFIG_WEBHOOK_SECRET = 'ZCASH_WEBHOOK_SECRET';
	const CONFIG_PAY_BASE_URL   = 'ZCASH_PAY_BASE_URL';
	const CONFIG_ZEC_RATE       = 'ZCASH_ZEC_RATE';
	const CONFIG_OS_AWAITING    = 'ZCASH_OS_AWAITING';
	const CONFIG_OS_HOLD        = 'ZCASH_OS_HOLD';

	/** @var array<int,int> Locks taken by hookActionOrderStatusUpdate, released after the change. */
	private $status_locks = array();

	public function __construct() {
		$this->name                   = 'zcashpayment';
		$this->tab                    = 'payments_gateways';
		$this->version                = '0.2.0';
		$this->author                 = 'nerdcash';
		$this->controllers            = array( 'validation', 'webhook' );
		$this->currencies             = true;
		$this->currencies_mode        = 'checkbox';
		$this->bootstrap              = true;
		$this->ps_versions_compliancy = array(
			'min' => '8.1.0',
			'max' => _PS_VERSION_,
		);
		parent::__construct();
		$this->displayName = $this->l( 'Zcash (self-hosted)' );
		$this->description = $this->l( 'Accept shielded Zcash payments through your own zcash-payment-service. No middleman, no fees.' );
	}

	public function install(): bool {
		if ( PHP_VERSION_ID < 80100 ) {
			$this->_errors[] = $this->l( 'This module needs PHP 8.1 or later.' );
			return false;
		}
		return parent::install()
			&& ZcashPaymentOrder::install()
			&& $this->install_states()
			&& $this->install_currency()
			&& $this->registerHook( 'paymentOptions' )
			&& $this->registerHook( 'displayPaymentReturn' )
			&& $this->registerHook( 'actionOrderStatusUpdate' )
			&& $this->registerHook( 'actionOrderStatusPostUpdate' )
			&& $this->registerHook( 'actionOrderSlipAdd' );
	}

	public function uninstall(): bool {
		// The order table and states stay: they record which address each order was given.
		foreach ( array( self::CONFIG_SERVICE_URL, self::CONFIG_WEBHOOK_SECRET, self::CONFIG_PAY_BASE_URL, self::CONFIG_ZEC_RATE ) as $key ) {
			Configuration::deleteByName( $key );
		}
		return parent::uninstall();
	}

	private function install_states(): bool {
		$states = array(
			self::CONFIG_OS_AWAITING => array( 'Awaiting Zcash payment', '#4169E1' ),
			self::CONFIG_OS_HOLD     => array( 'Zcash funds on hold', '#FF8C00' ),
		);
		foreach ( $states as $key => $def ) {
			$existing = (int) Configuration::get( $key );
			if ( $existing && Validate::isLoadedObject( new OrderState( $existing ) ) ) {
				continue;
			}
			$state              = new OrderState();
			$state->name        = array();
			foreach ( Language::getLanguages( false ) as $lang ) {
				$state->name[ (int) $lang['id_lang'] ] = $def[0];
			}
			$state->module_name = $this->name;
			$state->color       = $def[1];
			$state->send_email  = false;
			$state->logable     = false;
			$state->paid        = false;
			$state->invoice     = false;
			$state->unremovable = true;
			$state->hidden      = false;
			if ( ! $state->add() ) {
				return false;
			}
			Configuration::updateValue( $key, (int) $state->id );
		}
		return true;
	}

	/** Adds ZEC as an unofficial currency with 8 decimals, left disabled for the merchant to enable. */
	private function install_currency(): bool {
		$existing = (int) Currency::getIdByIsoCode( 'ZEC', 0, true );
		if ( $existing ) {
			return $this->allow_currency( $existing );
		}
		$currency                   = new Currency();
		$currency->iso_code         = 'ZEC';
		$currency->numeric_iso_code = '';
		$currency->precision        = 8;
		$currency->conversion_rate  = 1;
		$currency->unofficial       = true;
		$currency->active           = false;
		$currency->name             = array();
		$currency->symbol           = array();
		foreach ( Language::getLanguages( false ) as $lang ) {
			$currency->name[ (int) $lang['id_lang'] ]   = 'Zcash';
			$currency->symbol[ (int) $lang['id_lang'] ] = 'ZEC';
		}
		try {
			if ( $currency->add() ) {
				return $this->allow_currency( (int) $currency->id );
			}
			PrestaShopLogger::addLog( 'Zcash: could not add the ZEC currency.', 2 );
			return true;
		} catch ( Throwable $e ) {
			// Not fatal: the merchant can add ZEC under International > Localization > Currencies
			// and enable it for this module under Payment > Preferences.
			PrestaShopLogger::addLog( 'Zcash: could not add the ZEC currency: ' . $e->getMessage(), 2 );
			return true;
		}
	}

	/** Offers this module for carts in the currency (parent::install() only links the ones that existed). */
	private function allow_currency( int $id_currency ): bool {
		foreach ( Shop::getShops( false, null, true ) as $id_shop ) {
			Db::getInstance()->insert(
				'module_currency',
				array(
					'id_module'   => (int) $this->id,
					'id_shop'     => (int) $id_shop,
					'id_currency' => $id_currency,
				),
				false,
				false,
				Db::INSERT_IGNORE
			);
		}
		return true;
	}

	// ---------------------------------------------------------------- settings

	public static function option( string $key ): string {
		return trim( (string) Configuration::get( $key ) );
	}

	public function getContent(): string {
		$html = '';
		if ( Tools::isSubmit( 'submitZcashPayment' ) ) {
			$url = trim( (string) Tools::getValue( self::CONFIG_SERVICE_URL ) );
			$base = trim( (string) Tools::getValue( self::CONFIG_PAY_BASE_URL ) );
			$rate = trim( (string) Tools::getValue( self::CONFIG_ZEC_RATE ) );
			if ( '' !== $url && ! preg_match( '#^https?://#i', $url ) ) {
				$html .= $this->displayError( $this->l( 'The service URL must start with http:// or https://.' ) );
			} elseif ( '' !== $base && ! preg_match( '#^https?://#i', $base ) ) {
				$html .= $this->displayError( $this->l( 'The pay page base URL must start with http:// or https://.' ) );
			} elseif ( '' !== $rate && ( ! is_numeric( $rate ) || (float) $rate <= 0 ) ) {
				$html .= $this->displayError( $this->l( 'The exchange rate must be a positive number.' ) );
			} else {
				Configuration::updateValue( self::CONFIG_SERVICE_URL, $url );
				Configuration::updateValue( self::CONFIG_PAY_BASE_URL, $base );
				Configuration::updateValue( self::CONFIG_ZEC_RATE, $rate );
				$secret = (string) Tools::getValue( self::CONFIG_WEBHOOK_SECRET );
				if ( '' !== $secret ) {
					// Left blank means unchanged, so the secret is never echoed back into the form.
					Configuration::updateValue( self::CONFIG_WEBHOOK_SECRET, $secret );
				}
				$html .= $this->displayConfirmation( $this->l( 'Settings updated.' ) );
			}
		}
		$webhook = $this->context->link->getModuleLink( $this->name, 'webhook', array(), true );
		$html   .= '<div class="alert alert-info">' . $this->l( 'Set the service\'s ZCASH_WEBHOOK_URL to:' ) . ' <code>' . htmlspecialchars( $webhook, ENT_QUOTES ) . '</code></div>';

		$form            = array(
			'form' => array(
				'legend' => array( 'title' => $this->displayName ),
				'input'  => array(
					array(
						'type'     => 'text',
						'label'    => $this->l( 'Service URL' ),
						'name'     => self::CONFIG_SERVICE_URL,
						'desc'     => $this->l( 'Where PrestaShop reaches zcash-payment-service, for example http://zcash-payment-service:8787. It does not need to be public.' ),
						'required' => true,
					),
					array(
						'type' => 'password',
						'label' => $this->l( 'Webhook secret' ),
						'name' => self::CONFIG_WEBHOOK_SECRET,
						'desc' => $this->l( 'The service\'s ZCASH_WEBHOOK_SECRET. Leave blank to keep the saved one.' ),
					),
					array(
						'type'  => 'text',
						'label' => $this->l( 'Pay page base URL' ),
						'name'  => self::CONFIG_PAY_BASE_URL,
						'desc'  => $this->l( 'Only if the service does not set PUBLIC_BASE_URL.' ),
					),
					array(
						'type'  => 'text',
						'label' => $this->l( 'ZEC per unit of store currency' ),
						'name'  => self::CONFIG_ZEC_RATE,
						'desc'  => $this->l( 'Only for carts in the shop\'s default currency, when that is not ZEC. A fixed rate you keep current yourself; carts in other currencies are not offered Zcash.' ),
					),
				),
				'submit' => array( 'title' => $this->l( 'Save' ) ),
			),
		);
		$helper                = new HelperForm();
		$helper->module        = $this;
		$helper->submit_action = 'submitZcashPayment';
		$helper->currentIndex  = $this->context->link->getAdminLink( 'AdminModules', false ) . '&configure=' . $this->name;
		$helper->token         = Tools::getAdminTokenLite( 'AdminModules' );
		$helper->tpl_vars      = array(
			'fields_value' => array(
				self::CONFIG_SERVICE_URL    => self::option( self::CONFIG_SERVICE_URL ),
				self::CONFIG_WEBHOOK_SECRET => '',
				self::CONFIG_PAY_BASE_URL   => self::option( self::CONFIG_PAY_BASE_URL ),
				self::CONFIG_ZEC_RATE       => self::option( self::CONFIG_ZEC_RATE ),
			),
		);
		return $html . $helper->generateForm( array( $form ) );
	}

	// ---------------------------------------------------------------- order states

	public static function awaiting_state(): int {
		return (int) Configuration::get( self::CONFIG_OS_AWAITING );
	}

	public static function hold_state(): int {
		return (int) Configuration::get( self::CONFIG_OS_HOLD );
	}

	/** States a payment can complete. */
	public static function is_open( int $state ): bool {
		return $state > 0 && self::awaiting_state() === $state;
	}

	/** States that are never completed automatically: the goods may be gone. */
	public static function is_closed( int $state ): bool {
		return in_array( $state, array( (int) Configuration::get( 'PS_OS_CANCELED' ), (int) Configuration::get( 'PS_OS_REFUND' ), (int) Configuration::get( 'PS_OS_ERROR' ) ), true );
	}

	/** States where the order has been paid. */
	public static function is_paid( int $state ): bool {
		if ( $state <= 0 || self::hold_state() === $state ) {
			return false;
		}
		$os = new OrderState( $state );
		return Validate::isLoadedObject( $os ) && (bool) $os->paid;
	}

	/** Reads an order past the request-local object cache, so another request's change is seen. */
	public static function fresh_order( int $id_order ): ?Order {
		Cache::clean( 'objectmodel_Order_' . $id_order . '_*' );
		$order = new Order( $id_order );
		return Validate::isLoadedObject( $order ) ? $order : null;
	}

	public static function is_ours( Order $order ): bool {
		return 'zcashpayment' === $order->module;
	}

	public static function currency_iso( Order $order ): string {
		return strtoupper( (string) ( new Currency( (int) $order->id_currency ) )->iso_code );
	}

	/** The order total and currency as recorded when its address was issued. */
	public static function priced( Order $order ): string {
		return sprintf( '%.6F %s', (float) $order->total_paid, self::currency_iso( $order ) );
	}

	/** Moves an order to a state and reports whether it got there. */
	public static function set_state( int $id_order, int $state ): bool {
		$order = self::fresh_order( $id_order );
		if ( ! $order ) {
			return false;
		}
		if ( (int) $order->current_state !== $state ) {
			try {
				$order->setCurrentState( $state );
			} catch ( Throwable $e ) {
				PrestaShopLogger::addLog( 'Zcash: order ' . $id_order . ': state change failed: ' . $e->getMessage(), 3 );
				return false;
			}
		}
		$order = self::fresh_order( $id_order );
		return $order && (int) $order->current_state === $state;
	}

	/** Adds a private (merchant-only) message to an order. */
	public static function note( int $id_order, string $text ): void {
		$message           = new Message();
		$message->id_order = $id_order;
		$message->message  = $text;
		$message->private  = true;
		try {
			$message->add();
		} catch ( Throwable $e ) {
			PrestaShopLogger::addLog( 'Zcash: order ' . $id_order . ': ' . $text, 1 );
		}
	}

	/** Records the webhook's event id on the order's payments that have none. */
	public static function set_transaction_id( Order $order, string $transaction_id ): void {
		foreach ( $order->getOrderPaymentCollection() as $payment ) {
			if ( '' === (string) $payment->transaction_id ) {
				$payment->transaction_id = substr( $transaction_id, 0, 254 );
				$payment->update();
			}
		}
	}

	// ---------------------------------------------------------------- checkout

	/**
	 * Whether the gateway is configured and can price a cart in this currency. The one exchange
	 * rate describes the shop's default currency, so it is never applied to any other.
	 */
	public static function available_for( string $currency_iso ): bool {
		if ( '' === self::option( self::CONFIG_SERVICE_URL ) || '' === self::option( self::CONFIG_WEBHOOK_SECRET ) ) {
			return false;
		}
		$iso = strtoupper( $currency_iso );
		if ( 'ZEC' === $iso ) {
			return true;
		}
		$default = new Currency( (int) Configuration::get( 'PS_CURRENCY_DEFAULT' ) );
		return '' !== self::option( self::CONFIG_ZEC_RATE ) && Validate::isLoadedObject( $default ) && strtoupper( (string) $default->iso_code ) === $iso;
	}

	/**
	 * Whether the cart becomes a single order. PrestaShop splits a cart shipped in several
	 * packages into several orders, and one Zcash address pays exactly one order.
	 */
	public static function single_order( Cart $cart ): bool {
		$packages = 0;
		foreach ( $cart->getPackageList() as $per_address ) {
			$packages += count( $per_address );
		}
		return $packages <= 1;
	}

	public function hookPaymentOptions( $params ) {
		if ( ! $this->active || empty( $params['cart'] ) ) {
			return array();
		}
		$currency = new Currency( (int) $params['cart']->id_currency );
		if ( ! $this->checkCurrency( $params['cart'] ) || ! self::available_for( (string) $currency->iso_code ) || ! self::single_order( $params['cart'] ) ) {
			return array();
		}
		$option = new PaymentOption();
		$option->setModuleName( $this->name )
			->setCallToActionText( $this->l( 'Pay with Zcash (ZEC)' ) )
			->setAction( $this->context->link->getModuleLink( $this->name, 'validation', array(), true ) )
			->setAdditionalInformation( '<p>' . $this->l( 'Pay privately with shielded Zcash. After you place the order you will see a QR code to scan with your wallet.' ) . '</p>' );
		return array( $option );
	}

	private function checkCurrency( $cart ): bool {
		foreach ( (array) $this->getCurrency( (int) $cart->id_currency ) as $currency ) {
			if ( (int) $currency['id_currency'] === (int) $cart->id_currency ) {
				return true;
			}
		}
		return false;
	}

	public function confirmation_url( Order $order ): string {
		$customer = new Customer( (int) $order->id_customer );
		return $this->context->link->getPageLink(
			'order-confirmation',
			true,
			null,
			array(
				'id_cart'   => (int) $order->id_cart,
				'id_module' => (int) $this->id,
				'id_order'  => (int) $order->id,
				'key'       => $customer->secure_key,
			)
		);
	}

	/**
	 * Creates (or reuses) the order's payment session and returns where to send the payer, or
	 * an error message for the customer.
	 *
	 * @return array{url?:string,error?:string}
	 */
	public function start_session( int $id_order ): array {
		// Checkout and webhooks share one per-order lock, so two checkouts cannot both mint a
		// session and a webhook never sees a half-written one.
		$lock = 'zcash_ps_order_' . $id_order;
		if ( ! ZcashPaymentUtil::lock( $lock ) ) {
			return array( 'error' => $this->l( 'This order is busy. Please try again.' ) );
		}
		try {
			return $this->start_session_locked( $id_order );
		} catch ( ZcashPaymentDbException $e ) {
			// The address, if one was minted, was never shown or saved, so it stays unused.
			PrestaShopLogger::addLog( 'Zcash: ' . $e->getMessage(), 3 );
			return array( 'error' => $this->l( 'Zcash payments are temporarily unavailable. Please try again.' ) );
		} finally {
			ZcashPaymentUtil::unlock( $lock );
		}
	}

	private function start_session_locked( int $id_order ): array {
		$order = self::fresh_order( $id_order );
		if ( ! $order || ! self::is_ours( $order ) ) {
			return array( 'error' => $this->l( 'Order not found.' ) );
		}
		$priced = self::priced( $order );
		$row    = ZcashPaymentOrder::get( $id_order );
		$state  = (int) $order->current_state;

		// An order keeps one address for life: a payment to an address the payer was already
		// shown must always reach this order.
		if ( '' !== $row['address'] ) {
			if ( $row['needs_review'] || self::is_closed( $state ) ) {
				return array( 'error' => $this->l( 'A payment for this order is under review. Please contact the store.' ) );
			}
			if ( $row['priced'] === $priced ) {
				return array( 'url' => self::public_pay_url( $row['pay_url'] ) ?? $this->confirmation_url( $order ) );
			}
			self::note( $id_order, 'The order total changed after a Zcash address was issued. Reconcile it by hand.' );
			return array( 'error' => $this->l( 'This order changed after payment started. Please contact the store.' ) );
		}
		if ( ! self::is_open( $state ) ) {
			return array( 'error' => $this->l( 'This order can no longer be paid. Please contact the store.' ) );
		}

		$amount_zat = ZcashPaymentUtil::order_total_to_zat( sprintf( '%.6F', (float) $order->total_paid ), self::currency_iso( $order ), self::option( self::CONFIG_ZEC_RATE ) );
		if ( null === $amount_zat ) {
			return array( 'error' => $this->l( 'This order total cannot be converted to ZEC.' ) );
		}

		$unavailable = array( 'error' => $this->l( 'Zcash payments are temporarily unavailable. Please try again.' ) );
		$response    = self::http(
			'POST',
			'/payment-session',
			array(
				'payment_source' => 'zcash',
				'label'          => 'Order ' . $order->reference,
				'memo'           => 'Order ' . $order->reference,
				'message'        => (string) Configuration::get( 'PS_SHOP_NAME' ),
				'amount'         => ZcashPaymentUtil::zat_to_zec( $amount_zat ),
				'return_url'     => $this->confirmation_url( $order ),
			),
			15
		);
		if ( null === $response ) {
			return $unavailable;
		}
		$session = json_decode( $response['body'], true );
		if ( 200 !== $response['status'] || ! is_array( $session ) || empty( $session['address'] ) || ! is_string( $session['address'] )
			|| empty( $session['qr_text'] ) || ! is_string( $session['payment_id'] ?? null ) || ! ctype_xdigit( $session['payment_id'] ) ) {
			// The webhook confirms funds through GET /payments/{payment_id}, so a session needs one.
			PrestaShopLogger::addLog( 'Zcash: zcash-payment-service returned HTTP ' . $response['status'] . ': ' . substr( $response['body'], 0, 300 ), 3 );
			return $unavailable;
		}

		// The request took time and the lock only covers this module: re-read, and give up if
		// the order was closed, paid, or repriced meanwhile. The new address stays unused.
		$order = self::fresh_order( $id_order );
		if ( ! $order || ! self::is_ours( $order ) || ! self::is_open( (int) $order->current_state ) || self::priced( $order ) !== $priced ) {
			return array( 'error' => $this->l( 'This order changed while payment was starting. Please try again or contact the store.' ) );
		}
		ZcashPaymentOrder::set(
			$id_order,
			array(
				'address'      => (string) $session['address'],
				'qr_text'      => (string) $session['qr_text'],
				'payment_id'   => (string) $session['payment_id'],
				'pay_url'      => is_string( $session['pay_url'] ?? null ) ? $session['pay_url'] : '',
				'amount_zat'   => $amount_zat,
				'received_zat' => 0,
				'seen_zat'     => 0,
				'priced'       => $priced,
			)
		);
		self::note( $id_order, sprintf( 'Awaiting %s ZEC at %s', ZcashPaymentUtil::zat_to_zec( $amount_zat ), (string) $session['address'] ) );
		return array( 'url' => self::public_pay_url( is_string( $session['pay_url'] ?? null ) ? $session['pay_url'] : '' ) ?? $this->confirmation_url( $order ) );
	}

	/** Shows payment instructions on the order confirmation page while an order waits for Zcash. */
	public function hookDisplayPaymentReturn( $params ) {
		$order = $params['order'] ?? null;
		if ( ! $this->active || ! $order instanceof Order || ! self::is_ours( $order ) ) {
			return '';
		}
		$id_order = (int) $order->id;
		$order    = self::fresh_order( $id_order ) ?? $order;
		$state    = (int) $order->current_state;
		try {
			$row = ZcashPaymentOrder::get( $id_order );
		} catch ( ZcashPaymentDbException $e ) {
			return '<p>' . htmlspecialchars( $this->l( 'The payment status can\'t be checked right now. Refresh in a moment before sending anything.' ), ENT_QUOTES, 'UTF-8' ) . '</p>';
		}
		$h        = static function ( string $s ): string {
			return htmlspecialchars( $s, ENT_QUOTES, 'UTF-8' );
		};
		$out      = '<section class="zcash-payment-details"><h3>' . $h( $this->l( 'Pay with Zcash' ) ) . '</h3>';
		if ( self::is_paid( $state ) ) {
			return '<p class="zcash-payment-complete">' . $h( $this->l( 'Your Zcash payment has been received. Thank you!' ) ) . '</p>';
		}
		if ( '' === $row['address'] ) {
			return $out . '<p>' . $h( $this->l( 'The Zcash payment could not be started. Please contact the store.' ) ) . '</p></section>';
		}
		// Payments sent now would only be set aside by the webhook, so don't ask for one.
		if ( $row['needs_review'] || self::is_closed( $state ) || ( self::hold_state() === $state && ! $row['auto_hold'] ) || $row['priced'] !== self::priced( $order ) ) {
			return $out . '<p>' . $h( $this->l( 'This payment needs review by the store. Please contact the store before sending anything.' ) ) . '</p></section>';
		}
		$live = self::service_status( $id_order );
		if ( null === $live ) {
			// Our own total may be stale; don't risk asking for money already sent.
			return $out . '<p>' . $h( $this->l( 'The payment status can\'t be checked right now. Refresh in a moment before sending anything.' ) ) . '</p></section>';
		}
		if ( in_array( $live['status'] ?? '', array( 'paid', 'received' ), true ) ) {
			return $out . '<p class="zcash-payment-seen">' . $h( $this->l( 'Payment seen. Waiting for confirmation; refresh in a moment.' ) ) . '</p></section>';
		}
		$due       = (int) $row['amount_zat'];
		// Count what the customer has sent, confirmed or not, so they aren't asked to send it again.
		$received  = ZcashPaymentBalance::seen( $live ) ?? max( (int) $row['seen_zat'], (int) $row['received_zat'] );
		$remaining = $due - $received;
		if ( $due > 0 && $remaining <= 0 ) {
			return $out . '<p class="zcash-payment-seen">' . $h( $this->l( 'Payment received. Waiting for confirmation; refresh in a moment.' ) ) . '</p></section>';
		}
		// After a partial payment the service's request asks only for the rest.
		$qr_text = is_string( $live['qr_text'] ?? null ) && '' !== $live['qr_text'] ? $live['qr_text'] : $row['qr_text'];
		$out    .= '<p>' . $h( sprintf( $received > 0 ? $this->l( 'Send the remaining %s ZEC to:' ) : $this->l( 'Send %s ZEC to:' ), ZcashPaymentUtil::zat_to_zec( $remaining ) ) ) . '</p>';
		$out    .= '<p style="word-break:break-all"><code>' . $h( $row['address'] ) . '</code></p>';
		if ( '' !== $qr_text && 0 === strncasecmp( $qr_text, 'zcash:', 6 ) ) {
			$out .= '<p><a class="btn btn-primary" href="' . $h( $qr_text ) . '">' . $h( $this->l( 'Open in wallet' ) ) . '</a></p>';
		}
		$public = self::public_pay_url( $row['pay_url'] );
		if ( null !== $public ) {
			$out .= '<p><a href="' . $h( $public ) . '">' . $h( $this->l( 'Show the QR code' ) ) . '</a></p>';
		}
		return $out . '</section>';
	}

	// ---------------------------------------------------------------- order changes

	/**
	 * Before any state change to a Zcash order, takes the per-order lock the webhook holds while
	 * it checks and changes the order. A merchant's cancellation therefore waits for an
	 * in-flight webhook, which then sees the cancellation or finishes first. The lock is
	 * re-entrant, so the webhook's own state changes pass through.
	 */
	public function hookActionOrderStatusUpdate( $params ): void {
		$id_order = (int) ( $params['id_order'] ?? 0 );
		if ( ! $id_order || ! self::is_ours_by_id( $id_order ) ) {
			return;
		}
		if ( ! ZcashPaymentUtil::lock( 'zcash_ps_order_' . $id_order ) ) {
			// Refuse rather than race a payment being applied.
			throw new PrestaShopException( 'A Zcash payment for this order is being processed. Try again in a moment.' );
		}
		$this->status_locks[ $id_order ] = ( $this->status_locks[ $id_order ] ?? 0 ) + 1;
	}

	public function hookActionOrderStatusPostUpdate( $params ): void {
		$id_order = (int) ( $params['id_order'] ?? 0 );
		$new      = isset( $params['newOrderStatus'] ) && is_object( $params['newOrderStatus'] ) ? (int) $params['newOrderStatus']->id : 0;
		try {
			if ( $id_order && self::is_ours_by_id( $id_order ) ) {
				$this->after_status_change( $id_order, $new );
			}
		} catch ( ZcashPaymentDbException $e ) {
			// The state change itself already happened; don't fail the merchant's request.
			PrestaShopLogger::addLog( 'Zcash: order ' . $id_order . ': could not update after a state change: ' . $e->getMessage(), 3, null, 'Order', $id_order );
		} finally {
			if ( ! empty( $this->status_locks[ $id_order ] ) ) {
				--$this->status_locks[ $id_order ];
				ZcashPaymentUtil::unlock( 'zcash_ps_order_' . $id_order );
			}
		}
	}

	private function after_status_change( int $id_order, int $new ): void {
		$row = ZcashPaymentOrder::get( $id_order );
		if ( '' === $row['address'] ) {
			return;
		}
		$fields = array();
		if ( $row['auto_hold'] && self::hold_state() !== $new ) {
			// The module's hold has ended; a later hold is the merchant's own.
			$fields['auto_hold']      = 0;
			$fields['pre_hold_state'] = 0;
		}
		if ( self::is_closed( $new ) ) {
			// Funds may have reached the service while their webhook is still queued, so the
			// locally recorded balance can't rule them out.
			$fields['needs_review'] = 1;
		}
		if ( $fields ) {
			ZcashPaymentOrder::set( $id_order, $fields );
		}
	}

	/** A refund (credit slip), partial ones included, leaves funds the order may no longer be owed. */
	public function hookActionOrderSlipAdd( $params ): void {
		$order = $params['order'] ?? null;
		if ( ! $order instanceof Order || ! self::is_ours( $order ) ) {
			return;
		}
		$id_order = (int) $order->id;
		$lock     = 'zcash_ps_order_' . $id_order;
		// Wait for an in-flight webhook, so the flag lands before or after it, not in between.
		$locked = ZcashPaymentUtil::lock( $lock );
		try {
			if ( '' !== ZcashPaymentOrder::get( $id_order )['address'] ) {
				ZcashPaymentOrder::set( $id_order, array( 'needs_review' => 1 ) );
			}
		} catch ( ZcashPaymentDbException $e ) {
			PrestaShopLogger::addLog( 'Zcash: order ' . $id_order . ': could not flag a credit slip for review: ' . $e->getMessage(), 3, null, 'Order', $id_order );
		} finally {
			if ( $locked ) {
				ZcashPaymentUtil::unlock( $lock );
			}
		}
	}

	private static function is_ours_by_id( int $id_order ): bool {
		return 'zcashpayment' === (string) Db::getInstance()->getValue( 'SELECT `module` FROM `' . _DB_PREFIX_ . 'orders` WHERE `id_order` = ' . (int) $id_order, false );
	}

	// ---------------------------------------------------------------- service

	/** The pay page URL to send the payer to, or null when there is no public one. */
	public static function public_pay_url( string $pay_url ): ?string {
		return ZcashPaymentUtil::resolve_pay_url( $pay_url, self::option( self::CONFIG_PAY_BASE_URL ) );
	}

	/** The service's current status for this order's payment link, or null if unavailable. */
	public static function service_status( int $id_order ): ?array {
		try {
			$row = ZcashPaymentOrder::get( $id_order );
		} catch ( ZcashPaymentDbException $e ) {
			return null;
		}
		if ( '' === $row['payment_id'] || ! ctype_xdigit( $row['payment_id'] ) ) {
			return null;
		}
		$response = self::http( 'GET', '/payments/' . $row['payment_id'], null, 5 );
		if ( null === $response || 200 !== $response['status'] ) {
			return null;
		}
		$body = json_decode( $response['body'], true );
		// Only trust the service's view for this order's own address.
		return is_array( $body ) && hash_equals( $row['address'], (string) ( $body['address'] ?? '' ) ) ? $body : null;
	}

	/** @return array{status:int,body:string}|null */
	private static function http( string $method, string $path, ?array $json, int $timeout ): ?array {
		$ch = curl_init( rtrim( self::option( self::CONFIG_SERVICE_URL ), '/' ) . $path );
		$headers = array( 'Accept: application/json' );
		curl_setopt_array(
			$ch,
			array(
				CURLOPT_RETURNTRANSFER => true,
				CURLOPT_TIMEOUT        => $timeout,
				CURLOPT_CONNECTTIMEOUT => 5,
				CURLOPT_FOLLOWLOCATION => false,
				CURLOPT_CUSTOMREQUEST  => $method,
			)
		);
		if ( null !== $json ) {
			$headers[] = 'Content-Type: application/json';
			curl_setopt( $ch, CURLOPT_POSTFIELDS, json_encode( $json ) );
		}
		curl_setopt( $ch, CURLOPT_HTTPHEADER, $headers );
		$body = curl_exec( $ch );
		if ( false === $body ) {
			PrestaShopLogger::addLog( 'Zcash: zcash-payment-service unreachable: ' . curl_error( $ch ), 3 );
			return null;
		}
		return array(
			'status' => (int) curl_getinfo( $ch, CURLINFO_RESPONSE_CODE ),
			'body'   => (string) $body,
		);
	}
}
