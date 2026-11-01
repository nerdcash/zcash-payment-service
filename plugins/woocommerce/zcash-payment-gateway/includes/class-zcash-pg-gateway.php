<?php
/**
 * WooCommerce payment gateway backed by a self-hosted zcash-payment-service.
 */

defined( 'ABSPATH' ) || exit;

class Zcash_PG_Gateway extends WC_Payment_Gateway {
	const ID = 'zcash_payment';

	// Order meta keys.
	const META_ADDRESS        = '_zcash_address';
	const META_PAYMENT_ID     = '_zcash_payment_id';
	const META_PAY_URL        = '_zcash_pay_url';
	const META_QR_TEXT        = '_zcash_qr_text';
	const META_AMOUNT_ZAT     = '_zcash_amount_zat';
	const META_RECEIVED_ZAT   = '_zcash_received_zat';
	const META_STATE_VERSION  = '_zcash_state_version';
	const META_PRICED         = '_zcash_priced_total';
	const META_NEEDS_REVIEW   = '_zcash_needs_review';
	const META_AUTO_HOLD      = '_zcash_auto_hold';
	const META_PRE_HOLD       = '_zcash_pre_hold_status';

	public function __construct() {
		$this->id                 = self::ID;
		$this->method_title       = __( 'Zcash (self-hosted)', 'zcash-payment-gateway' );
		$this->method_description = __( 'Shielded Zcash payments through your own zcash-payment-service. Payments go straight to your wallet. No middleman, no fees.', 'zcash-payment-gateway' );
		$this->has_fields         = false;
		$this->supports           = array( 'products' );

		$this->init_form_fields();
		$this->init_settings();

		$this->title       = $this->get_option( 'title' );
		$this->description = $this->get_option( 'description' );
		$this->enabled     = $this->get_option( 'enabled' );

		add_action( 'woocommerce_update_options_payment_gateways_' . $this->id, array( $this, 'process_admin_options' ) );
		add_action( 'woocommerce_thankyou_' . $this->id, array( $this, 'thankyou_page' ) );
	}

	public function init_form_fields() {
		$this->form_fields = array(
			'enabled'        => array(
				'title'   => __( 'Enable', 'zcash-payment-gateway' ),
				'type'    => 'checkbox',
				'label'   => __( 'Accept Zcash', 'zcash-payment-gateway' ),
				'default' => 'no',
			),
			'title'          => array(
				'title'   => __( 'Title', 'zcash-payment-gateway' ),
				'type'    => 'text',
				'default' => __( 'Zcash (ZEC)', 'zcash-payment-gateway' ),
			),
			'description'    => array(
				'title'   => __( 'Description', 'zcash-payment-gateway' ),
				'type'    => 'textarea',
				'default' => __( 'Pay privately with shielded Zcash. You will see a QR code to scan with your wallet.', 'zcash-payment-gateway' ),
			),
			'service_url'    => array(
				'title'       => __( 'Service URL', 'zcash-payment-gateway' ),
				'type'        => 'text',
				'description' => __( 'Private URL WordPress uses to reach zcash-payment-service, for example http://zcash-payment-service:8787. Keep this off the public internet.', 'zcash-payment-gateway' ),
				'default'     => 'http://zcash-payment-service:8787',
			),
			'webhook_secret' => array(
				'title'       => __( 'Webhook secret', 'zcash-payment-gateway' ),
				'type'        => 'password',
				'description' => sprintf(
					/* translators: %s: webhook URL */
					__( 'The same value as ZCASH_WEBHOOK_SECRET on the service. Set the service\'s ZCASH_WEBHOOK_URL to %s', 'zcash-payment-gateway' ),
					'<code>' . esc_html( Zcash_PG_Webhook::url() ) . '</code>'
				),
				'default'     => '',
			),
			'pay_base_url'   => array(
				'title'       => __( 'Pay page base URL', 'zcash-payment-gateway' ),
				'type'        => 'text',
				'description' => __( 'Public URL where customers reach the service\'s pay page, for example https://pay.example.com. Leave empty if the service sets PUBLIC_BASE_URL. Without either, customers see the address on the order-received page instead.', 'zcash-payment-gateway' ),
				'default'     => '',
			),
			'zec_rate'       => array(
				'title'       => __( 'ZEC per unit of store currency', 'zcash-payment-gateway' ),
				'type'        => 'text',
				'description' => __( 'Only used when the store currency is not ZEC. For example 0.025 if 1 USD buys 0.025 ZEC. The plugin calls no price service. Keep this current yourself, or set the store currency to Zcash (ZEC).', 'zcash-payment-gateway' ),
				'default'     => '',
			),
		);
	}

	public function is_available() {
		if ( ! parent::is_available() ) {
			return false;
		}
		if ( '' === trim( (string) $this->get_option( 'service_url' ) ) || '' === (string) $this->get_option( 'webhook_secret' ) ) {
			return false;
		}
		$rate = $this->get_option( 'zec_rate' );
		if ( 'ZEC' !== get_woocommerce_currency() && ! ( is_numeric( $rate ) && (float) $rate > 0 ) ) {
			return false;
		}
		return true;
	}

	public function process_payment( $order_id ) {
		$order = wc_get_order( $order_id );
		if ( ! $order ) {
			return $this->fail( __( 'Order not found.', 'zcash-payment-gateway' ) );
		}

		// Checkout and webhooks share one per-order lock, so two checkouts cannot both mint a
		// session and a webhook never sees a half-written one.
		$lock = 'zcash_pg_order_' . $order->get_id();
		if ( ! Zcash_PG_Util::lock( $lock ) ) {
			return $this->fail( __( 'This order is busy. Please try again.', 'zcash-payment-gateway' ) );
		}
		try {
			$order = wc_get_order( $order_id );
			return $order ? $this->start_session( $order ) : $this->fail( __( 'Order not found.', 'zcash-payment-gateway' ) );
		} finally {
			Zcash_PG_Util::unlock( $lock );
		}
	}

	/** Creates (or reuses) the order's payment session. Runs under the per-order lock. */
	private function start_session( WC_Order $order ) {
		// An order keeps one address for life: a payment to an address the payer was already
		// shown must always reach this order. Reuse it when the price is unchanged; if the
		// order was repriced, stop and let the merchant reconcile instead of minting another.
		$priced = $order->get_total() . ' ' . $order->get_currency();
		// A failed order without a Zcash session (for example a declined card) may retry with
		// Zcash; only a failure that happens during the service request below stops checkout.
		$was_failed = $order->has_status( 'failed' );
		if ( $order->get_meta( self::META_ADDRESS ) ) {
			if ( $order->get_meta( self::META_NEEDS_REVIEW ) || $order->has_status( array( 'cancelled', 'failed', 'refunded' ) ) ) {
				// Funds at this address were set aside for manual review. Reusing the session
				// would wait forever for a balance change that already happened.
				return $this->fail( __( 'A payment for this order is under review. Please contact the store.', 'zcash-payment-gateway' ) );
			}
			if ( (string) $order->get_meta( self::META_PRICED ) === $priced ) {
				return array(
					'result'   => 'success',
					'redirect' => $this->public_pay_url( (string) $order->get_meta( self::META_PAY_URL ) ) ?? $this->get_return_url( $order ),
				);
			}
			$order->add_order_note( __( 'The order total changed after a Zcash address was issued. Reconcile it by hand.', 'zcash-payment-gateway' ) );
			return $this->fail( __( 'This order changed after payment started. Please contact the store.', 'zcash-payment-gateway' ) );
		}

		$amount_zat = Zcash_PG_Util::order_total_to_zat(
			(string) $order->get_total(),
			(string) $order->get_currency(),
			(string) $this->get_option( 'zec_rate' )
		);
		if ( null === $amount_zat ) {
			return $this->fail( __( 'This order total cannot be converted to ZEC.', 'zcash-payment-gateway' ) );
		}

		$body = array(
			'payment_source' => 'zcash',
			'label'          => sprintf( /* translators: %s: order number */ __( 'Order %s', 'zcash-payment-gateway' ), $order->get_order_number() ),
			'memo'           => sprintf( 'Order %s', $order->get_order_number() ),
			'message'        => get_bloginfo( 'name' ),
			'amount'         => Zcash_PG_Util::zat_to_zec( $amount_zat ),
			'return_url'     => $this->get_return_url( $order ),
		);

		$response = wp_remote_post(
			$this->service_endpoint( '/payment-session' ),
			array(
				'timeout' => 15,
				'headers' => array(
					'Content-Type' => 'application/json',
					'Accept'       => 'application/json',
				),
				'body'    => wp_json_encode( $body ),
			)
		);
		if ( is_wp_error( $response ) ) {
			wc_get_logger()->error( 'zcash-payment-service unreachable: ' . $response->get_error_message(), array( 'source' => self::ID ) );
			return $this->fail( __( 'Zcash payments are temporarily unavailable. Please try again.', 'zcash-payment-gateway' ) );
		}
		$status  = (int) wp_remote_retrieve_response_code( $response );
		$session = json_decode( (string) wp_remote_retrieve_body( $response ), true );
		if ( 200 !== $status || ! is_array( $session ) || empty( $session['address'] ) || empty( $session['qr_text'] )
			|| ! is_string( $session['payment_id'] ?? null ) || ! ctype_xdigit( $session['payment_id'] ) ) {
			// The webhook confirms funds through GET /payments/{payment_id}, so a session needs one.
			wc_get_logger()->error( 'zcash-payment-service returned HTTP ' . $status . ': ' . substr( (string) wp_remote_retrieve_body( $response ), 0, 300 ), array( 'source' => self::ID ) );
			return $this->fail( __( 'Zcash payments are temporarily unavailable. Please try again.', 'zcash-payment-gateway' ) );
		}

		// The request took time and the lock only covers this plugin: re-read, and give up if
		// the order was closed or repriced meanwhile. The new address stays unused.
		$order = wc_get_order( $order->get_id() );
		// A hold placed meanwhile is the merchant's, and a switch to another gateway abandons
		// this session; neither may be overwritten by the pending transition below.
		$closed = array( 'cancelled', 'refunded', 'on-hold' );
		if ( ! $was_failed ) {
			$closed[] = 'failed';
		}
		if ( ! $order || $order->is_paid() || $order->has_status( $closed )
			|| self::ID !== $order->get_payment_method()
			|| $order->get_total() . ' ' . $order->get_currency() !== $priced ) {
			return $this->fail( __( 'This order changed while payment was starting. Please try again or contact the store.', 'zcash-payment-gateway' ) );
		}
		$order->update_meta_data( self::META_ADDRESS, (string) $session['address'] );
		$order->update_meta_data( self::META_QR_TEXT, (string) $session['qr_text'] );
		$order->update_meta_data( self::META_AMOUNT_ZAT, (string) $amount_zat );
		$order->update_meta_data( self::META_PRICED, $priced );
		$order->update_meta_data( self::META_RECEIVED_ZAT, '0' );
		$order->update_meta_data( self::META_STATE_VERSION, '0' );
		if ( ! empty( $session['payment_id'] ) ) {
			$order->update_meta_data( self::META_PAYMENT_ID, (string) $session['payment_id'] );
		}
		if ( ! empty( $session['pay_url'] ) ) {
			$order->update_meta_data( self::META_PAY_URL, (string) $session['pay_url'] );
		}
		$order->update_status(
			'pending',
			sprintf(
				/* translators: 1: ZEC amount, 2: address */
				__( 'Awaiting %1$s ZEC at %2$s.', 'zcash-payment-gateway' ),
				Zcash_PG_Util::zat_to_zec( $amount_zat ),
				(string) $session['address']
			)
		);
		$order->save();

		if ( function_exists( 'WC' ) && WC()->cart ) {
			WC()->cart->empty_cart();
		}

		$redirect = $this->public_pay_url( (string) ( $session['pay_url'] ?? '' ) ) ?? $this->get_return_url( $order );

		return array(
			'result'   => 'success',
			'redirect' => $redirect,
		);
	}

	/**
	 * Fallback on the order-received page: the address and a wallet link, in case the payer
	 * lands here before paying (for example with an older service that has no pay page).
	 */
	public function thankyou_page( $order_id ) {
		$order = wc_get_order( $order_id );
		if ( ! $order || $order->is_paid() || $order->has_status( array( 'cancelled', 'failed', 'refunded' ) ) ) {
			return;
		}
		$address = (string) $order->get_meta( self::META_ADDRESS );
		$qr_text = (string) $order->get_meta( self::META_QR_TEXT );
		$pay_url = (string) $order->get_meta( self::META_PAY_URL );
		if ( '' === $address ) {
			return;
		}
		echo '<section class="zcash-payment-details"><h2>' . esc_html__( 'Pay with Zcash', 'zcash-payment-gateway' ) . '</h2>';
		// Payments sent now would only be set aside by the webhook, so don't ask for one.
		if ( $order->get_meta( self::META_NEEDS_REVIEW )
			|| ( $order->has_status( 'on-hold' ) && ! $order->get_meta( self::META_AUTO_HOLD ) )
			|| (string) $order->get_meta( self::META_PRICED ) !== $order->get_total() . ' ' . $order->get_currency() ) {
			echo '<p>' . esc_html__( 'This payment needs review by the store. Please contact the store before sending anything.', 'zcash-payment-gateway' ) . '</p></section>';
			return;
		}
		$due      = (int) $order->get_meta( self::META_AMOUNT_ZAT );
		$received = (int) $order->get_meta( self::META_RECEIVED_ZAT );
		// The service's view is newer than ours while a webhook is queued or retrying.
		$live = $this->service_status( $order );
		if ( null === $live ) {
			// Our own total may be stale; don't risk asking for money already sent.
			echo '<p>' . esc_html__( 'The payment status can\'t be checked right now. Refresh in a moment before sending anything.', 'zcash-payment-gateway' ) . '</p></section>';
			return;
		}
		{
			if ( in_array( $live['status'] ?? '', array( 'paid', 'received' ), true ) ) {
				echo '<p>' . esc_html__( 'Payment seen. Waiting for confirmation; refresh in a moment.', 'zcash-payment-gateway' ) . '</p></section>';
				return;
			}
			$live_zat = Zcash_PG_Util::zec_to_zat( (string) ( $live['total_received'] ?? '' ) );
			if ( null !== $live_zat ) {
				$received = $live_zat;
			}
			if ( ! empty( $live['qr_text'] ) && is_string( $live['qr_text'] ) ) {
				// After a partial payment the service's request asks only for the rest.
				$qr_text = $live['qr_text'];
			}
		}
		$remaining = $due - $received;
		if ( $due > 0 && $remaining <= 0 ) {
			echo '<p>' . esc_html__( 'Payment received. Waiting for confirmation; this page will show the order as paid shortly.', 'zcash-payment-gateway' ) . '</p></section>';
			return;
		}
		echo '<p>' . esc_html(
			sprintf(
				/* translators: %s: ZEC amount */
				$received > 0 ? __( 'Send the remaining %s ZEC to:', 'zcash-payment-gateway' ) : __( 'Send %s ZEC to:', 'zcash-payment-gateway' ),
				Zcash_PG_Util::zat_to_zec( $remaining )
			)
		) . '</p>';
		echo '<p style="word-break:break-all"><code>' . esc_html( $address ) . '</code></p>';
		if ( '' !== $qr_text ) {
			$uri = ( $received > 0 && null === $live ) ? 'zcash:' . $address . '?amount=' . rtrim( rtrim( Zcash_PG_Util::zat_to_zec( $remaining ), '0' ), '.' ) : $qr_text;
			echo '<p><a class="button" href="' . esc_attr( $uri ) . '">' . esc_html__( 'Open in wallet', 'zcash-payment-gateway' ) . '</a></p>';
		}
		$public = $this->public_pay_url( $pay_url );
		if ( null !== $public ) {
			echo '<p><a href="' . esc_url( $public ) . '">' . esc_html__( 'Show the QR code', 'zcash-payment-gateway' ) . '</a></p>';
		}
		echo '</section>';
	}

	/**
	 * The pay page URL to send the payer to, or null when there is no public one.
	 * An absolute pay_url (service has PUBLIC_BASE_URL) is used as-is. A relative one is joined to
	 * the "Pay page base URL" setting.
	 */
	private function public_pay_url( string $pay_url ): ?string {
		return Zcash_PG_Util::resolve_pay_url( $pay_url, (string) $this->get_option( 'pay_base_url' ) );
	}

	/** The service's current status for this order's payment link, or null if unavailable. */
	public function service_status( WC_Order $order ): ?array {
		$payment_id = (string) $order->get_meta( self::META_PAYMENT_ID );
		if ( '' === $payment_id || ! ctype_xdigit( $payment_id ) ) {
			return null;
		}
		$response = wp_remote_get( $this->service_endpoint( '/payments/' . $payment_id ), array( 'timeout' => 5 ) );
		if ( is_wp_error( $response ) || 200 !== (int) wp_remote_retrieve_response_code( $response ) ) {
			return null;
		}
		$body = json_decode( (string) wp_remote_retrieve_body( $response ), true );
		// Only trust the service's view for this order's own address.
		return is_array( $body ) && hash_equals( (string) $order->get_meta( self::META_ADDRESS ), (string) ( $body['address'] ?? '' ) ) ? $body : null;
	}

	private function service_endpoint( string $path ): string {
		return rtrim( (string) $this->get_option( 'service_url' ), '/' ) . $path;
	}

	private function fail( string $message ): array {
		wc_add_notice( $message, 'error' );
		return array( 'result' => 'failure' );
	}
}
