<?php
/**
 * Easy Digital Downloads gateway backed by a self-hosted zcash-payment-service.
 */

defined( 'ABSPATH' ) || exit;

final class Zcash_EDD_Gateway {
	const ID = 'zcash';

	// Order meta keys.
	const META_ADDRESS       = '_zcash_address';
	const META_PAYMENT_ID    = '_zcash_payment_id';
	const META_PAY_URL       = '_zcash_pay_url';
	const META_QR_TEXT       = '_zcash_qr_text';
	const META_AMOUNT_ZAT    = '_zcash_amount_zat';
	const META_RECEIVED_ZAT  = '_zcash_received_zat';
	const META_STATE_VERSION = '_zcash_state_version';
	const META_PRICED        = '_zcash_priced_total';
	const META_NEEDS_REVIEW  = '_zcash_needs_review';
	const META_AUTO_HOLD     = '_zcash_auto_hold';
	const META_PRE_HOLD      = '_zcash_pre_hold_status';

	/** Statuses where the order has been paid. */
	const PAID_STATUSES = array( 'complete', 'partially_refunded' );
	/** Statuses that are never completed automatically: their downloads may already be revoked. */
	const CLOSED_STATUSES = array( 'refunded', 'revoked', 'failed', 'abandoned' );
	/** Statuses a payment can complete. */
	const OPEN_STATUSES = array( 'pending', 'processing' );

	public static function register(): void {
		add_filter(
			'edd_payment_gateways',
			static function ( $gateways ) {
				$gateways[ self::ID ] = array(
					'admin_label'    => __( 'Zcash (self-hosted)', 'zcash-payment-edd' ),
					'checkout_label' => self::option( 'title' ) ?: __( 'Zcash (ZEC)', 'zcash-payment-edd' ),
					'supports'       => array(),
				);
				return $gateways;
			}
		);
		// Hide the gateway at checkout until it can take a payment.
		add_filter(
			'edd_enabled_payment_gateways',
			static function ( $gateways ) {
				if ( isset( $gateways[ self::ID ] ) && ! is_admin() && ! self::is_configured() ) {
					unset( $gateways[ self::ID ] );
				}
				return $gateways;
			}
		);
		add_action( 'edd_' . self::ID . '_cc_form', array( self::class, 'checkout_description' ) );
		add_action( 'edd_gateway_' . self::ID, array( self::class, 'process' ) );
		add_filter(
			'edd_settings_sections_gateways',
			static function ( $sections ) {
				$sections[ self::ID ] = __( 'Zcash', 'zcash-payment-edd' );
				return $sections;
			}
		);
		add_filter( 'edd_settings_gateways', array( self::class, 'settings' ) );
		add_action( 'edd_order_receipt_after_table', array( self::class, 'receipt' ), 10, 1 );
	}

	public static function settings( $settings ) {
		$settings[ self::ID ] = array(
			'zcash_header'         => array(
				'id'   => 'zcash_header',
				'name' => '<strong>' . __( 'Zcash (self-hosted)', 'zcash-payment-edd' ) . '</strong>',
				'type' => 'header',
			),
			'zcash_title'          => array(
				'id'   => 'zcash_title',
				'name' => __( 'Title', 'zcash-payment-edd' ),
				'desc' => __( 'Shown to customers at checkout. Defaults to "Zcash (ZEC)".', 'zcash-payment-edd' ),
				'type' => 'text',
				'std'  => '',
			),
			'zcash_service_url'    => array(
				'id'   => 'zcash_service_url',
				'name' => __( 'Service URL', 'zcash-payment-edd' ),
				'desc' => __( 'Private URL WordPress uses to reach zcash-payment-service, for example http://zcash-payment-service:8787. Keep this off the public internet.', 'zcash-payment-edd' ),
				'type' => 'text',
				'std'  => 'http://zcash-payment-service:8787',
			),
			'zcash_webhook_secret' => array(
				'id'   => 'zcash_webhook_secret',
				'name' => __( 'Webhook secret', 'zcash-payment-edd' ),
				'desc' => sprintf(
					/* translators: %s: webhook URL */
					__( 'The same value as ZCASH_WEBHOOK_SECRET on the service. Set the service\'s ZCASH_WEBHOOK_URL to %s', 'zcash-payment-edd' ),
					'<code>' . esc_html( Zcash_EDD_Webhook::url() ) . '</code>'
				),
				'type' => 'password',
			),
			'zcash_pay_base_url'   => array(
				'id'   => 'zcash_pay_base_url',
				'name' => __( 'Pay page base URL', 'zcash-payment-edd' ),
				'desc' => __( 'Public URL where customers reach the service\'s pay page, for example https://pay.example.com. Leave empty if the service sets PUBLIC_BASE_URL. Without either, customers see the address on the receipt page instead.', 'zcash-payment-edd' ),
				'type' => 'text',
			),
			'zcash_zec_rate'       => array(
				'id'   => 'zcash_zec_rate',
				'name' => __( 'ZEC per unit of store currency', 'zcash-payment-edd' ),
				'desc' => __( 'Only used when the store currency is not ZEC. For example 0.025 if 1 USD buys 0.025 ZEC. The plugin calls no price service. Keep this current yourself, or set the store currency to Zcash (ZEC).', 'zcash-payment-edd' ),
				'type' => 'text',
			),
		);
		return $settings;
	}

	public static function option( string $key ): string {
		return trim( (string) edd_get_option( 'zcash_' . $key, '' ) );
	}

	public static function is_configured(): bool {
		if ( '' === self::option( 'service_url' ) || '' === self::option( 'webhook_secret' ) ) {
			return false;
		}
		$rate = self::option( 'zec_rate' );
		return 'ZEC' === strtoupper( (string) edd_get_currency() ) || ( is_numeric( $rate ) && (float) $rate > 0 );
	}

	public static function checkout_description(): void {
		echo '<p class="zcash-checkout-description">' . esc_html__( 'Pay privately with shielded Zcash. After you place the order you will see a QR code to scan with your wallet.', 'zcash-payment-edd' ) . '</p>';
	}

	/**
	 * Reads an order past WordPress's request-local caches, so a change another request made
	 * meanwhile (a merchant's hold or revocation, another webhook delivery) is seen.
	 */
	public static function fresh_order( int $order_id ) {
		wp_cache_delete( $order_id, 'edd-orders' );
		wp_cache_delete( $order_id, 'edd_order_meta' );
		return edd_get_order( $order_id );
	}

	public static function meta( int $order_id, string $key ): string {
		return (string) edd_get_order_meta( $order_id, $key, true );
	}

	public static function set_meta( int $order_id, string $key, string $value ): void {
		edd_update_order_meta( $order_id, $key, $value );
	}

	public static function delete_meta( int $order_id, string $key ): void {
		edd_delete_order_meta( $order_id, $key );
	}

	public static function priced( $order ): string {
		return $order->total . ' ' . strtoupper( (string) $order->currency );
	}

	public static function note( int $order_id, string $note ): void {
		edd_add_note(
			array(
				'object_id'   => $order_id,
				'object_type' => 'order',
				'content'     => $note,
				'user_id'     => 0,
			)
		);
	}

	/** Runs for a checkout that chose Zcash. */
	public static function process( $purchase_data ): void {
		if ( ! wp_verify_nonce( $purchase_data['gateway_nonce'] ?? '', 'edd-gateway' ) ) {
			wp_die( esc_html__( 'Nonce verification has failed', 'zcash-payment-edd' ), esc_html__( 'Error', 'zcash-payment-edd' ), array( 'response' => 403 ) );
		}
		$back = '?payment-mode=' . self::ID;

		// Check the amount converts before recording an order.
		if ( null === Zcash_EDD_Util::order_total_to_zat( (string) $purchase_data['price'], (string) edd_get_currency(), self::option( 'zec_rate' ) ) ) {
			edd_set_error( 'zcash_amount', __( 'This order total cannot be converted to ZEC.', 'zcash-payment-edd' ) );
			edd_send_back_to_checkout( $back );
			return;
		}

		$payment_data = array(
			'price'        => $purchase_data['price'],
			'date'         => $purchase_data['date'] ?? '',
			'user_email'   => $purchase_data['user_email'],
			'purchase_key' => $purchase_data['purchase_key'],
			'currency'     => edd_get_currency(),
			'downloads'    => $purchase_data['downloads'],
			'user_info'    => $purchase_data['user_info'],
			'cart_details' => $purchase_data['cart_details'],
			'gateway'      => self::ID,
			'status'       => 'pending',
		);
		if ( '' === $payment_data['date'] ) {
			unset( $payment_data['date'] );
		}
		$order_id = edd_build_order( $payment_data );
		if ( ! $order_id ) {
			edd_record_gateway_error( __( 'Payment Error', 'zcash-payment-edd' ), __( 'Could not record a pending order for a Zcash payment.', 'zcash-payment-edd' ) );
			edd_set_error( 'zcash_order', __( 'Your order could not be recorded. Please try again.', 'zcash-payment-edd' ) );
			edd_send_back_to_checkout( $back );
			return;
		}
		// EDD 3.7.2+ refuses an order whose discount uses could not be claimed.
		if ( class_exists( '\EDD\Discounts\OrderValidation' ) && \EDD\Discounts\OrderValidation::refuse_unclaimed( $order_id ) ) {
			edd_send_back_to_checkout( $back );
			return;
		}

		$result = self::start_session( (int) $order_id );
		if ( is_wp_error( $result ) ) {
			// Fail the order only if it is still an open Zcash order with no address, so nothing
			// can be paid to it; a status someone else set meanwhile is kept.
			$order = self::fresh_order( (int) $order_id );
			if ( $order && self::ID === $order->gateway && in_array( $order->status, self::OPEN_STATUSES, true )
				&& '' === self::meta( (int) $order_id, self::META_ADDRESS ) ) {
				edd_update_order_status( $order_id, 'failed' );
			}
			edd_set_error( 'zcash_session', $result->get_error_message() );
			edd_send_back_to_checkout( $back );
			return;
		}
		edd_empty_cart();
		wp_redirect( $result ); // phpcs:ignore WordPress.Security.SafeRedirect -- the service's own pay page.
		exit;
	}

	/**
	 * Creates (or reuses) the order's payment session and returns where to send the payer.
	 *
	 * @return string|WP_Error
	 */
	public static function start_session( int $order_id ) {
		// Checkout and webhooks share one per-order lock, so two checkouts cannot both mint a
		// session and a webhook never sees a half-written one.
		$lock = 'zcash_edd_order_' . $order_id;
		if ( ! Zcash_EDD_Util::lock( $lock ) ) {
			return new WP_Error( 'zcash_busy', __( 'This order is busy. Please try again.', 'zcash-payment-edd' ) );
		}
		try {
			return self::start_session_locked( $order_id );
		} finally {
			Zcash_EDD_Util::unlock( $lock );
		}
	}

	private static function start_session_locked( int $order_id ) {
		$order = edd_get_order( $order_id );
		if ( ! $order || self::ID !== $order->gateway ) {
			return new WP_Error( 'zcash_order', __( 'Order not found.', 'zcash-payment-edd' ) );
		}
		$priced = self::priced( $order );

		// An order keeps one address for life: a payment to an address the payer was already
		// shown must always reach this order.
		if ( '' !== self::meta( $order_id, self::META_ADDRESS ) ) {
			if ( self::meta( $order_id, self::META_NEEDS_REVIEW ) || in_array( $order->status, self::CLOSED_STATUSES, true ) ) {
				return new WP_Error( 'zcash_review', __( 'A payment for this order is under review. Please contact the store.', 'zcash-payment-edd' ) );
			}
			if ( self::meta( $order_id, self::META_PRICED ) === $priced ) {
				return self::public_pay_url( self::meta( $order_id, self::META_PAY_URL ) ) ?? edd_get_receipt_page_uri( $order_id );
			}
			self::note( $order_id, __( 'The order total changed after a Zcash address was issued. Reconcile it by hand.', 'zcash-payment-edd' ) );
			return new WP_Error( 'zcash_repriced', __( 'This order changed after payment started. Please contact the store.', 'zcash-payment-edd' ) );
		}
		if ( ! in_array( $order->status, self::OPEN_STATUSES, true ) ) {
			return new WP_Error( 'zcash_closed', __( 'This order can no longer be paid. Please contact the store.', 'zcash-payment-edd' ) );
		}

		$amount_zat = Zcash_EDD_Util::order_total_to_zat( (string) $order->total, (string) $order->currency, self::option( 'zec_rate' ) );
		if ( null === $amount_zat ) {
			return new WP_Error( 'zcash_amount', __( 'This order total cannot be converted to ZEC.', 'zcash-payment-edd' ) );
		}

		$number   = (string) $order->get_number();
		$response = wp_remote_post(
			self::service_endpoint( '/payment-session' ),
			array(
				'timeout' => 15,
				'headers' => array(
					'Content-Type' => 'application/json',
					'Accept'       => 'application/json',
				),
				'body'    => wp_json_encode(
					array(
						'payment_source' => 'zcash',
						/* translators: %s: order number */
						'label'          => sprintf( __( 'Order %s', 'zcash-payment-edd' ), $number ),
						'memo'           => sprintf( 'Order %s', $number ),
						'message'        => get_bloginfo( 'name' ),
						'amount'         => Zcash_EDD_Util::zat_to_zec( $amount_zat ),
						'return_url'     => edd_get_receipt_page_uri( $order_id ),
					)
				),
			)
		);
		$unavailable = new WP_Error( 'zcash_unavailable', __( 'Zcash payments are temporarily unavailable. Please try again.', 'zcash-payment-edd' ) );
		if ( is_wp_error( $response ) ) {
			edd_debug_log( 'Zcash: zcash-payment-service unreachable: ' . $response->get_error_message() );
			return $unavailable;
		}
		$status  = (int) wp_remote_retrieve_response_code( $response );
		$session = json_decode( (string) wp_remote_retrieve_body( $response ), true );
		if ( 200 !== $status || ! is_array( $session ) || empty( $session['address'] ) || empty( $session['qr_text'] )
			|| ! is_string( $session['payment_id'] ?? null ) || ! ctype_xdigit( $session['payment_id'] ) ) {
			// The webhook confirms funds through GET /payments/{payment_id}, so a session needs one.
			edd_debug_log( 'Zcash: zcash-payment-service returned HTTP ' . $status . ': ' . substr( (string) wp_remote_retrieve_body( $response ), 0, 300 ) );
			return $unavailable;
		}

		// The request took time and the lock only covers this plugin: re-read, and give up if
		// the order was closed, paid, or repriced meanwhile. The new address stays unused.
		$order = self::fresh_order( $order_id );
		if ( ! $order || ! in_array( $order->status, self::OPEN_STATUSES, true ) || self::ID !== $order->gateway || self::priced( $order ) !== $priced ) {
			return new WP_Error( 'zcash_changed', __( 'This order changed while payment was starting. Please try again or contact the store.', 'zcash-payment-edd' ) );
		}
		self::set_meta( $order_id, self::META_ADDRESS, (string) $session['address'] );
		self::set_meta( $order_id, self::META_QR_TEXT, (string) $session['qr_text'] );
		self::set_meta( $order_id, self::META_PAYMENT_ID, (string) $session['payment_id'] );
		self::set_meta( $order_id, self::META_PAY_URL, is_string( $session['pay_url'] ?? null ) ? $session['pay_url'] : '' );
		self::set_meta( $order_id, self::META_AMOUNT_ZAT, (string) $amount_zat );
		self::set_meta( $order_id, self::META_RECEIVED_ZAT, '0' );
		self::set_meta( $order_id, self::META_PRICED, $priced );
		self::note(
			$order_id,
			sprintf(
				/* translators: 1: ZEC amount, 2: address */
				__( 'Awaiting %1$s ZEC at %2$s', 'zcash-payment-edd' ),
				Zcash_EDD_Util::zat_to_zec( $amount_zat ),
				(string) $session['address']
			)
		);
		return self::public_pay_url( self::meta( $order_id, self::META_PAY_URL ) ) ?? edd_get_receipt_page_uri( $order_id );
	}

	/** Shows payment instructions on the receipt while an order waits for Zcash. */
	public static function receipt( $order ): void {
		if ( ! is_object( $order ) || self::ID !== ( $order->gateway ?? '' ) ) {
			return;
		}
		$order_id = (int) $order->id;
		$address  = self::meta( $order_id, self::META_ADDRESS );
		if ( '' === $address || in_array( $order->status, self::PAID_STATUSES, true ) ) {
			return;
		}
		echo '<section class="zcash-payment-details"><h3>' . esc_html__( 'Pay with Zcash', 'zcash-payment-edd' ) . '</h3>';
		// Payments sent now would only be set aside by the webhook, so don't ask for one.
		if ( self::meta( $order_id, self::META_NEEDS_REVIEW )
			|| in_array( $order->status, self::CLOSED_STATUSES, true )
			|| ( 'on_hold' === $order->status && ! self::meta( $order_id, self::META_AUTO_HOLD ) )
			|| self::meta( $order_id, self::META_PRICED ) !== self::priced( $order ) ) {
			echo '<p>' . esc_html__( 'This payment needs review by the store. Please contact the store before sending anything.', 'zcash-payment-edd' ) . '</p></section>';
			return;
		}
		$due  = (int) self::meta( $order_id, self::META_AMOUNT_ZAT );
		$live = self::service_status( $order_id );
		if ( null === $live ) {
			// Our own total may be stale; don't risk asking for money already sent.
			echo '<p>' . esc_html__( 'The payment status can\'t be checked right now. Refresh in a moment before sending anything.', 'zcash-payment-edd' ) . '</p></section>';
			return;
		}
		if ( in_array( $live['status'] ?? '', array( 'paid', 'received' ), true ) ) {
			echo '<p class="zcash-payment-seen">' . esc_html__( 'Payment seen. Waiting for confirmation; refresh in a moment.', 'zcash-payment-edd' ) . '</p></section>';
			return;
		}
		$received = Zcash_EDD_Util::zec_to_zat( (string) ( $live['total_received'] ?? '' ) ) ?? (int) self::meta( $order_id, self::META_RECEIVED_ZAT );
		// After a partial payment the service's request asks only for the rest.
		$qr_text   = is_string( $live['qr_text'] ?? null ) && '' !== $live['qr_text'] ? $live['qr_text'] : self::meta( $order_id, self::META_QR_TEXT );
		$remaining = $due - $received;
		if ( $due > 0 && $remaining <= 0 ) {
			echo '<p class="zcash-payment-seen">' . esc_html__( 'Payment received. Waiting for confirmation; refresh in a moment.', 'zcash-payment-edd' ) . '</p></section>';
			return;
		}
		echo '<p>' . esc_html(
			sprintf(
				/* translators: %s: ZEC amount */
				$received > 0 ? __( 'Send the remaining %s ZEC to:', 'zcash-payment-edd' ) : __( 'Send %s ZEC to:', 'zcash-payment-edd' ),
				Zcash_EDD_Util::zat_to_zec( $remaining )
			)
		) . '</p>';
		echo '<p style="word-break:break-all"><code>' . esc_html( $address ) . '</code></p>';
		if ( '' !== $qr_text ) {
			echo '<p><a class="button" href="' . esc_attr( $qr_text ) . '">' . esc_html__( 'Open in wallet', 'zcash-payment-edd' ) . '</a></p>';
		}
		$public = self::public_pay_url( self::meta( $order_id, self::META_PAY_URL ) );
		if ( null !== $public ) {
			echo '<p><a href="' . esc_url( $public ) . '">' . esc_html__( 'Show the QR code', 'zcash-payment-edd' ) . '</a></p>';
		}
		echo '</section>';
	}

	/** The pay page URL to send the payer to, or null when there is no public one. */
	public static function public_pay_url( string $pay_url ): ?string {
		return Zcash_EDD_Util::resolve_pay_url( $pay_url, self::option( 'pay_base_url' ) );
	}

	/** The service's current status for this order's payment link, or null if unavailable. */
	public static function service_status( int $order_id ): ?array {
		$payment_id = self::meta( $order_id, self::META_PAYMENT_ID );
		if ( '' === $payment_id || ! ctype_xdigit( $payment_id ) ) {
			return null;
		}
		$response = wp_remote_get( self::service_endpoint( '/payments/' . $payment_id ), array( 'timeout' => 5 ) );
		if ( is_wp_error( $response ) || 200 !== (int) wp_remote_retrieve_response_code( $response ) ) {
			return null;
		}
		$body = json_decode( (string) wp_remote_retrieve_body( $response ), true );
		// Only trust the service's view for this order's own address.
		return is_array( $body ) && hash_equals( self::meta( $order_id, self::META_ADDRESS ), (string) ( $body['address'] ?? '' ) ) ? $body : null;
	}

	private static function service_endpoint( string $path ): string {
		return rtrim( self::option( 'service_url' ), '/' ) . $path;
	}
}
