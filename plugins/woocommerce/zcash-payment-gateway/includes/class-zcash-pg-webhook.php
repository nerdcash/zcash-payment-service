<?php
/**
 * Receives signed payment observations from zcash-payment-service and updates orders.
 */

defined( 'ABSPATH' ) || exit;

final class Zcash_PG_Webhook {
	const ROUTE_NAMESPACE = 'zcash-payment/v1';
	const ROUTE           = '/webhook';

	public static function register(): void {
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
		$gateway = self::gateway();
		$secret  = $gateway ? (string) $gateway->get_option( 'webhook_secret' ) : '';
		$raw     = (string) $request->get_body();

		if ( ! Zcash_PG_Util::verify_signature( $raw, $request->get_header( 'x-signature' ), $secret ) ) {
			return new WP_REST_Response( array( 'error' => 'invalid signature' ), 401 );
		}
		$observation = Zcash_PG_Util::parse_observation( $raw );
		if ( null === $observation ) {
			return new WP_REST_Response( array( 'error' => 'invalid payload' ), 400 );
		}

		$order = self::find_order( $observation['address'] );
		if ( null === $order ) {
			// Not ours (or deleted). Acknowledge so the service stops retrying.
			return new WP_REST_Response( array( 'ok' => true, 'ignored' => 'unknown address' ), 200 );
		}

		// Serialize deliveries for one order so the version check and the update are atomic.
		$lock = 'zcash_pg_order_' . $order->get_id();
		if ( ! self::lock( $lock ) ) {
			return new WP_REST_Response( array( 'error' => 'busy, retry' ), 503 );
		}
		try {
			return self::apply( $order->get_id(), $observation );
		} finally {
			self::unlock( $lock );
		}
	}

	/** Applies one observation to an order. Runs under the per-order lock. */
	private static function apply( int $order_id, array $observation ): WP_REST_Response {
		// Re-read inside the lock so we see any delivery that finished while we waited.
		$order = wc_get_order( $order_id );
		if ( ! $order ) {
			return new WP_REST_Response( array( 'ok' => true, 'ignored' => 'unknown address' ), 200 );
		}

		// Deliveries can arrive out of order. Apply only a newer state_version.
		$stored_version = (int) $order->get_meta( Zcash_PG_Gateway::META_STATE_VERSION );
		if ( $observation['state_version'] <= $stored_version ) {
			return new WP_REST_Response( array( 'ok' => true, 'ignored' => 'stale' ), 200 );
		}

		$total = $observation['total_zat'];
		$due   = (int) $order->get_meta( Zcash_PG_Gateway::META_AMOUNT_ZAT );
		$zec   = Zcash_PG_Util::zat_to_zec( $total );

		if ( $order->is_paid() ) {
			if ( $due > 0 && $total < $due ) {
				// A receipt was dropped (expired from the mempool or undone by a reorg).
				$message = sprintf(
					/* translators: 1: received ZEC, 2: due ZEC */
					__( 'Zcash funds for this paid order dropped to %1$s of %2$s ZEC (a transaction was dropped or reorganized out). Order put on hold; check before fulfilling.', 'zcash-payment-gateway' ),
					$zec,
					Zcash_PG_Util::zat_to_zec( $due )
				);
				wc_get_logger()->warning( 'Order ' . $order->get_id() . ': ' . $message, array( 'source' => Zcash_PG_Gateway::ID ) );
				$order->update_status( 'on-hold', $message );
			} else {
				/* translators: %s: ZEC total */
				$order->add_order_note( sprintf( __( 'Zcash total for this order is now %s ZEC.', 'zcash-payment-gateway' ), $zec ) );
			}
		} elseif ( $order->has_status( array( 'cancelled', 'failed', 'refunded' ) ) ) {
			// Never revive a closed order automatically: its stock may already be released.
			$message = sprintf(
				/* translators: 1: received ZEC, 2: order status */
				__( 'Received %1$s ZEC for this order after it was %2$s. Not completed automatically; review stock and refund or reinstate the order by hand.', 'zcash-payment-gateway' ),
				$zec,
				$order->get_status()
			);
			wc_get_logger()->warning( 'Order ' . $order->get_id() . ': ' . $message, array( 'source' => Zcash_PG_Gateway::ID ) );
			$order->add_order_note( $message );
		} elseif ( $due > 0 && $total >= $due ) {
			$order->update_meta_data( Zcash_PG_Gateway::META_RECEIVED_ZAT, (string) $total );
			$order->save();
			if ( ! $order->payment_complete( $observation['event_id'] ) ) {
				// Leave the version unchanged so the service's retry is applied.
				return new WP_REST_Response( array( 'error' => 'could not complete payment' ), 500 );
			}
			/* translators: %s: ZEC total */
			$order->add_order_note( sprintf( __( 'Received %s ZEC. Payment complete.', 'zcash-payment-gateway' ), $zec ) );
		} else {
			$order->add_order_note(
				sprintf(
					/* translators: 1: received ZEC, 2: due ZEC */
					__( 'Received %1$s of %2$s ZEC so far.', 'zcash-payment-gateway' ),
					$zec,
					Zcash_PG_Util::zat_to_zec( $due )
				)
			);
		}

		// Record the version last, after the transition above succeeded.
		$order->update_meta_data( Zcash_PG_Gateway::META_RECEIVED_ZAT, (string) $total );
		$order->update_meta_data( Zcash_PG_Gateway::META_STATE_VERSION, (string) $observation['state_version'] );
		$order->save();

		return new WP_REST_Response( array( 'ok' => true ), 200 );
	}

	/** Finds the order an address was issued for, on both HPOS and legacy post storage. */
	private static function find_order( string $address ): ?WC_Order {
		$args = array(
			'limit'  => 1,
			'status' => array_keys( wc_get_order_statuses() ),
		);
		if ( class_exists( '\Automattic\WooCommerce\Utilities\OrderUtil' )
			&& \Automattic\WooCommerce\Utilities\OrderUtil::custom_orders_table_usage_is_enabled() ) {
			$args['meta_query'] = array( // phpcs:ignore WordPress.DB.SlowDBQuery
				array(
					'key'   => Zcash_PG_Gateway::META_ADDRESS,
					'value' => $address,
				),
			);
		} else {
			// The legacy store passes these through to WP_Query.
			$args['meta_key']   = Zcash_PG_Gateway::META_ADDRESS; // phpcs:ignore WordPress.DB.SlowDBQuery
			$args['meta_value'] = $address; // phpcs:ignore WordPress.DB.SlowDBQuery
		}
		$orders = wc_get_orders( $args );
		$order  = $orders[0] ?? null;
		// Never trust the query alone: check the address really matches.
		if ( ! $order instanceof WC_Order || ! hash_equals( (string) $order->get_meta( Zcash_PG_Gateway::META_ADDRESS ), $address ) ) {
			return null;
		}
		return $order;
	}

	private static function lock( string $name ): bool {
		global $wpdb;
		return '1' === (string) $wpdb->get_var( $wpdb->prepare( 'SELECT GET_LOCK(%s, 10)', $name ) );
	}

	private static function unlock( string $name ): void {
		global $wpdb;
		$wpdb->query( $wpdb->prepare( 'SELECT RELEASE_LOCK(%s)', $name ) );
	}

	private static function gateway(): ?Zcash_PG_Gateway {
		$gateways = WC()->payment_gateways() ? WC()->payment_gateways()->payment_gateways() : array();
		$gateway  = $gateways[ Zcash_PG_Gateway::ID ] ?? null;
		return $gateway instanceof Zcash_PG_Gateway ? $gateway : null;
	}
}
