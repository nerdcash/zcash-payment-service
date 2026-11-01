<?php
/**
 * Receives signed payment observations from zcash-payment-service and updates orders.
 */

defined( 'ABSPATH' ) || exit;

final class Zcash_EDD_Webhook {
	const ROUTE_NAMESPACE = 'zcash-payment-edd/v1';
	const ROUTE           = '/webhook';

	public static function register(): void {
		// A refund or a closed order that holds Zcash funds sets those funds aside, so reopening
		// the order later can't reuse them automatically. Refunds, partial ones included, leave
		// funds the order may no longer be owed.
		add_action(
			'edd_refund_order',
			static function ( $order_id ) {
				if ( '' !== Zcash_EDD_Gateway::meta( (int) $order_id, Zcash_EDD_Gateway::META_ADDRESS ) ) {
					Zcash_EDD_Gateway::set_meta( (int) $order_id, Zcash_EDD_Gateway::META_NEEDS_REVIEW, '1' );
				}
			}
		);
		add_action(
			'edd_update_payment_status',
			static function ( $order_id, $new_status, $old_status ) {
				$order_id = (int) $order_id;
				if ( '' === Zcash_EDD_Gateway::meta( $order_id, Zcash_EDD_Gateway::META_ADDRESS ) ) {
					return;
				}
				if ( 'on_hold' === $old_status && Zcash_EDD_Gateway::meta( $order_id, Zcash_EDD_Gateway::META_AUTO_HOLD ) ) {
					// The plugin's hold has ended; a later hold is the merchant's own.
					Zcash_EDD_Gateway::delete_meta( $order_id, Zcash_EDD_Gateway::META_AUTO_HOLD );
					Zcash_EDD_Gateway::delete_meta( $order_id, Zcash_EDD_Gateway::META_PRE_HOLD );
				}
				if ( in_array( $new_status, Zcash_EDD_Gateway::CLOSED_STATUSES, true ) ) {
					// Funds may have reached the service while their webhook is still queued, so
					// the locally recorded balance can't rule them out.
					Zcash_EDD_Gateway::set_meta( $order_id, Zcash_EDD_Gateway::META_NEEDS_REVIEW, '1' );
				}
			},
			10,
			3
		);
		add_action(
			'rest_api_init',
			static function () {
				register_rest_route(
					self::ROUTE_NAMESPACE,
					self::ROUTE,
					array(
						'methods'             => 'POST',
						'callback'            => array( self::class, 'handle' ),
						// Authenticated by the HMAC signature in handle().
						'permission_callback' => '__return_true',
					)
				);
			}
		);
	}

	public static function url(): string {
		return rest_url( self::ROUTE_NAMESPACE . self::ROUTE );
	}

	public static function handle( WP_REST_Request $request ) {
		$raw = (string) $request->get_body();
		if ( ! Zcash_EDD_Util::verify_signature( $raw, $request->get_header( 'x-signature' ), Zcash_EDD_Gateway::option( 'webhook_secret' ) ) ) {
			return new WP_REST_Response( array( 'error' => 'invalid signature' ), 401 );
		}
		$observation = Zcash_EDD_Util::parse_observation( $raw );
		if ( null === $observation ) {
			return new WP_REST_Response( array( 'error' => 'invalid payload' ), 400 );
		}
		$order_id = self::find_order( $observation['address'] );
		if ( null === $order_id ) {
			// Not ours (or deleted). Acknowledge so the service stops retrying.
			return new WP_REST_Response( array( 'ok' => true, 'ignored' => 'unknown address' ), 200 );
		}

		// Serialize deliveries for one order so the version check and the update are atomic.
		$lock = 'zcash_edd_order_' . $order_id;
		if ( ! Zcash_EDD_Util::lock( $lock ) ) {
			return new WP_REST_Response( array( 'error' => 'busy, retry' ), 503 );
		}
		try {
			return self::apply( $order_id, $observation );
		} finally {
			Zcash_EDD_Util::unlock( $lock );
		}
	}

	/** Applies one observation to an order. Runs under the per-order lock. */
	private static function apply( int $order_id, array $observation ): WP_REST_Response {
		$g = Zcash_EDD_Gateway::class;
		// Re-read inside the lock so we see any delivery that finished while we waited.
		$order = edd_get_order( $order_id );
		if ( ! $order || ! hash_equals( $g::meta( $order_id, $g::META_ADDRESS ), $observation['address'] ) ) {
			return new WP_REST_Response( array( 'ok' => true, 'ignored' => 'unknown address' ), 200 );
		}

		// Deliveries can arrive out of order. Apply only a newer state_version.
		if ( $observation['state_version'] <= (int) $g::meta( $order_id, $g::META_STATE_VERSION ) ) {
			return new WP_REST_Response( array( 'ok' => true, 'ignored' => 'stale' ), 200 );
		}

		$total   = $observation['total_zat'];
		$due     = (int) $g::meta( $order_id, $g::META_AMOUNT_ZAT );
		$zec     = Zcash_EDD_Util::zat_to_zec( $total );
		$due_zec = Zcash_EDD_Util::zat_to_zec( $due );
		$paid    = in_array( $order->status, $g::PAID_STATUSES, true );

		$review_note = null;
		if ( 'on_hold' === $order->status && ! $g::meta( $order_id, $g::META_AUTO_HOLD ) ) {
			// A hold the merchant set: leave it for them.
			/* translators: %s: received ZEC */
			$review_note = sprintf( __( 'Zcash total for this order is now %s ZEC. The order is on hold, so it was not completed automatically.', 'zcash-payment-edd' ), $zec );
		} elseif ( $g::meta( $order_id, $g::META_NEEDS_REVIEW ) ) {
			// Once funds are set aside for review, keep later receipts there too until the
			// merchant clears the flag, so reconciled or refunded funds never complete the order.
			/* translators: %s: received ZEC */
			$review_note = sprintf( __( 'Zcash total for this order is now %s ZEC. A payment for this order is under manual review, so this was not applied automatically.', 'zcash-payment-edd' ), $zec );
		} elseif ( $g::ID !== $order->gateway ) {
			/* translators: %s: received ZEC */
			$review_note = sprintf( __( 'Received %s ZEC at this order\'s abandoned Zcash address, but the order now uses another payment method. Not applied; refund or reconcile by hand.', 'zcash-payment-edd' ), $zec );
		} elseif ( ! $paid && $g::meta( $order_id, $g::META_PRICED ) !== $g::priced( $order ) ) {
			/* translators: %s: received ZEC */
			$review_note = sprintf( __( 'Received %s ZEC, but the order total changed after the Zcash amount was set. Not completed automatically; reconcile by hand.', 'zcash-payment-edd' ), $zec );
		}

		if ( null !== $review_note ) {
			edd_debug_log( 'Zcash: order ' . $order_id . ': ' . $review_note );
			$g::note( $order_id, $review_note );
			$g::set_meta( $order_id, $g::META_NEEDS_REVIEW, '1' );
			if ( $paid && $due > 0 && $total < $due && $g::ID === $order->gateway ) {
				// Review doesn't make an underfunded Zcash-paid order safe to fulfill: hold it too.
				/* translators: 1: received ZEC, 2: due ZEC */
				if ( ! self::auto_hold( $order, sprintf( __( 'Zcash funds for this paid order dropped to %1$s of %2$s ZEC. Order put on hold; check before fulfilling.', 'zcash-payment-edd' ), $zec, $due_zec ) ) ) {
					return new WP_REST_Response( array( 'error' => 'could not hold order, retry' ), 500 );
				}
			}
		} elseif ( $paid ) {
			if ( $due > 0 && $total < $due ) {
				// A receipt was dropped (expired from the mempool or undone by a reorg).
				/* translators: 1: received ZEC, 2: due ZEC */
				$message = sprintf( __( 'Zcash funds for this paid order dropped to %1$s of %2$s ZEC (a transaction was dropped or reorganized out). Order put on hold; check before fulfilling.', 'zcash-payment-edd' ), $zec, $due_zec );
				edd_debug_log( 'Zcash: order ' . $order_id . ': ' . $message );
				if ( ! self::auto_hold( $order, $message ) ) {
					return new WP_REST_Response( array( 'error' => 'could not hold order, retry' ), 500 );
				}
			} else {
				/* translators: %s: ZEC total */
				$g::note( $order_id, sprintf( __( 'Zcash total for this order is now %s ZEC.', 'zcash-payment-edd' ), $zec ) );
			}
		} elseif ( in_array( $order->status, $g::CLOSED_STATUSES, true ) ) {
			// Never revive a closed order automatically: its downloads may already be revoked.
			/* translators: 1: received ZEC, 2: order status */
			$message = sprintf( __( 'Received %1$s ZEC for this order after it was %2$s. Not completed automatically; refund or reinstate the order by hand.', 'zcash-payment-edd' ), $zec, $order->status );
			edd_debug_log( 'Zcash: order ' . $order_id . ': ' . $message );
			$g::note( $order_id, $message );
			$g::set_meta( $order_id, $g::META_NEEDS_REVIEW, '1' );
		} elseif ( $due > 0 && $total >= $due ) {
			// Deliveries can be retried after a newer state was skipped (for example a receipt
			// that was later dropped). Confirm the service still sees the funds first.
			$live     = $g::service_status( $order_id );
			$live_zat = null === $live ? null : Zcash_EDD_Util::zec_to_zat( (string) ( $live['total_received'] ?? '' ) );
			if ( null === $live_zat ) {
				return new WP_REST_Response( array( 'error' => 'cannot confirm with the service, retry' ), 503 );
			}
			if ( $live_zat < $due ) {
				/* translators: 1: reported ZEC, 2: current ZEC */
				$g::note( $order_id, sprintf( __( 'A payment event reported %1$s ZEC, but the service now sees %2$s ZEC. Not completed.', 'zcash-payment-edd' ), $zec, Zcash_EDD_Util::zat_to_zec( $live_zat ) ) );
				return new WP_REST_Response( array( 'ok' => true, 'ignored' => 'superseded' ), 200 );
			}
			// The status request took time: re-read so a change made meanwhile wins.
			$order    = edd_get_order( $order_id );
			$pre_hold = $order ? $g::meta( $order_id, $g::META_PRE_HOLD ) : '';
			$auto     = $order && 'on_hold' === $order->status && $g::meta( $order_id, $g::META_AUTO_HOLD );
			if ( ! $order || $g::ID !== $order->gateway || $g::meta( $order_id, $g::META_NEEDS_REVIEW )
				|| $g::meta( $order_id, $g::META_PRICED ) !== $g::priced( $order )
				|| ! ( in_array( $order->status, $g::OPEN_STATUSES, true ) || $auto ) ) {
				// The retry re-evaluates from scratch.
				return new WP_REST_Response( array( 'error' => 'order changed, retry' ), 503 );
			}
			$g::set_meta( $order_id, $g::META_RECEIVED_ZAT, (string) $total );
			// Recovering from the plugin's own hold restores the earlier status; otherwise complete.
			$target = $auto && '' !== $pre_hold ? $pre_hold : 'complete';
			if ( ! edd_update_order_status( $order_id, $target ) ) {
				// Leave the version unchanged so the service's retry is applied.
				return new WP_REST_Response( array( 'error' => 'could not complete payment' ), 500 );
			}
			if ( 'complete' === $target && ! $auto ) {
				edd_set_payment_transaction_id( $order_id, $observation['event_id'] );
				/* translators: %s: ZEC total */
				$g::note( $order_id, sprintf( __( 'Received %s ZEC. Payment complete.', 'zcash-payment-edd' ), $zec ) );
			} else {
				/* translators: %s: ZEC total */
				$g::note( $order_id, sprintf( __( 'Zcash funds recovered to %s ZEC. Hold released.', 'zcash-payment-edd' ), $zec ) );
			}
		} else {
			/* translators: 1: received ZEC, 2: due ZEC */
			$g::note( $order_id, sprintf( __( 'Received %1$s of %2$s ZEC so far.', 'zcash-payment-edd' ), $zec, $due_zec ) );
		}

		// Record the version last, after the transition above succeeded.
		$g::set_meta( $order_id, $g::META_RECEIVED_ZAT, (string) $total );
		$g::set_meta( $order_id, $g::META_STATE_VERSION, (string) $observation['state_version'] );
		return new WP_REST_Response( array( 'ok' => true ), 200 );
	}

	/**
	 * Puts a paid order on hold, remembering its status so recovery can restore it. Returns false
	 * if the transition failed, so the caller can ask for a retry before recording the event.
	 */
	private static function auto_hold( $order, string $message ): bool {
		$g        = Zcash_EDD_Gateway::class;
		$order_id = (int) $order->id;
		$g::note( $order_id, $message );
		if ( 'on_hold' === $order->status ) {
			return true;
		}
		$g::set_meta( $order_id, $g::META_AUTO_HOLD, '1' );
		$g::set_meta( $order_id, $g::META_PRE_HOLD, (string) $order->status );
		if ( edd_update_order_status( $order_id, 'on_hold' ) ) {
			return true;
		}
		// Don't leave hold markers on an order that isn't held.
		$g::delete_meta( $order_id, $g::META_AUTO_HOLD );
		$g::delete_meta( $order_id, $g::META_PRE_HOLD );
		return false;
	}

	/** Finds the order an address was issued for. */
	private static function find_order( string $address ): ?int {
		global $wpdb;
		$order_id = $wpdb->get_var(
			$wpdb->prepare(
				"SELECT edd_order_id FROM {$wpdb->prefix}edd_ordermeta WHERE meta_key = %s AND meta_value = %s ORDER BY edd_order_id DESC LIMIT 1",
				Zcash_EDD_Gateway::META_ADDRESS,
				$address
			)
		);
		if ( null === $order_id ) {
			return null;
		}
		// Never trust the query alone: check the address really matches.
		return hash_equals( Zcash_EDD_Gateway::meta( (int) $order_id, Zcash_EDD_Gateway::META_ADDRESS ), $address ) ? (int) $order_id : null;
	}
}
