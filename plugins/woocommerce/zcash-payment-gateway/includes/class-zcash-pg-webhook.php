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

		$orders = wc_get_orders(
			array(
				'limit'      => 1,
				'meta_query' => array( // phpcs:ignore WordPress.DB.SlowDBQuery
					array(
						'key'   => Zcash_PG_Gateway::META_ADDRESS,
						'value' => $observation['address'],
					),
				),
				'status'     => array_keys( wc_get_order_statuses() ),
			)
		);
		if ( empty( $orders ) ) {
			// Not ours (or deleted). Acknowledge so the service stops retrying.
			return new WP_REST_Response( array( 'ok' => true, 'ignored' => 'unknown address' ), 200 );
		}
		$order = $orders[0];

		// Deliveries can arrive out of order. Apply only a newer state_version.
		$stored_version = (int) $order->get_meta( Zcash_PG_Gateway::META_STATE_VERSION );
		if ( $observation['state_version'] <= $stored_version ) {
			return new WP_REST_Response( array( 'ok' => true, 'ignored' => 'stale' ), 200 );
		}

		$order->update_meta_data( Zcash_PG_Gateway::META_STATE_VERSION, (string) $observation['state_version'] );
		$order->update_meta_data( Zcash_PG_Gateway::META_RECEIVED_ZAT, (string) $observation['total_zat'] );
		$due = (int) $order->get_meta( Zcash_PG_Gateway::META_AMOUNT_ZAT );

		if ( $order->is_paid() ) {
			$order->add_order_note(
				sprintf(
					/* translators: %s: ZEC total */
					__( 'Zcash total for this order is now %s ZEC.', 'zcash-payment-gateway' ),
					Zcash_PG_Util::zat_to_zec( $observation['total_zat'] )
				)
			);
			$order->save();
		} elseif ( $due > 0 && $observation['total_zat'] >= $due ) {
			$order->save();
			$order->payment_complete( $observation['event_id'] );
			$order->add_order_note(
				sprintf(
					/* translators: %s: ZEC total */
					__( 'Received %s ZEC. Payment complete.', 'zcash-payment-gateway' ),
					Zcash_PG_Util::zat_to_zec( $observation['total_zat'] )
				)
			);
		} else {
			$order->add_order_note(
				sprintf(
					/* translators: 1: received ZEC, 2: due ZEC */
					__( 'Received %1$s of %2$s ZEC so far.', 'zcash-payment-gateway' ),
					Zcash_PG_Util::zat_to_zec( $observation['total_zat'] ),
					Zcash_PG_Util::zat_to_zec( $due )
				)
			);
			$order->save();
		}

		return new WP_REST_Response( array( 'ok' => true ), 200 );
	}

	private static function gateway(): ?Zcash_PG_Gateway {
		$gateways = WC()->payment_gateways() ? WC()->payment_gateways()->payment_gateways() : array();
		$gateway  = $gateways[ Zcash_PG_Gateway::ID ] ?? null;
		return $gateway instanceof Zcash_PG_Gateway ? $gateway : null;
	}
}
