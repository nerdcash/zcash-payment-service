<?php
/**
 * Applies signed payment observations from zcash-payment-service to orders.
 */

if ( ! defined( '_PS_VERSION_' ) ) {
	exit;
}

final class ZcashPaymentWebhook {
	/**
	 * Handles one delivery.
	 *
	 * @return array{0:int,1:array<string,mixed>} HTTP status and JSON body.
	 */
	public static function handle( string $raw, ?string $signature ): array {
		if ( ! ZcashPaymentUtil::verify_signature( $raw, $signature, ZcashPayment::option( ZcashPayment::CONFIG_WEBHOOK_SECRET ) ) ) {
			return array( 401, array( 'error' => 'invalid signature' ) );
		}
		$observation = ZcashPaymentUtil::parse_observation( $raw );
		if ( null === $observation ) {
			return array( 400, array( 'error' => 'invalid payload' ) );
		}
		$id_order = ZcashPaymentOrder::find_by_address( $observation['address'] );
		if ( false === $id_order ) {
			// A database error is not an unknown address: have the service retry.
			return array( 503, array( 'error' => 'lookup failed, retry' ) );
		}
		if ( null === $id_order ) {
			// Not ours (or deleted). Acknowledge so the service stops retrying.
			return array( 200, array( 'ok' => true, 'ignored' => 'unknown address' ) );
		}

		// Serialize deliveries for one order so the version check and the update are atomic.
		$lock = 'zcash_ps_order_' . $id_order;
		if ( ! ZcashPaymentUtil::lock( $lock ) ) {
			return array( 503, array( 'error' => 'busy, retry' ) );
		}
		try {
			return self::apply( $id_order, $observation );
		} catch ( ZcashPaymentDbException $e ) {
			// Nothing after the failed read or write ran; the version is unsaved, so a retry
			// re-evaluates from scratch.
			PrestaShopLogger::addLog( 'Zcash: webhook: ' . $e->getMessage(), 3 );
			return array( 503, array( 'error' => 'database error, retry' ) );
		} finally {
			ZcashPaymentUtil::unlock( $lock );
		}
	}

	/** Applies one observation to an order. Runs under the per-order lock. */
	private static function apply( int $id_order, array $observation ): array {
		$g = ZcashPayment::class;
		// Re-read inside the lock so we see any delivery that finished while we waited.
		$order = $g::fresh_order( $id_order );
		$row   = ZcashPaymentOrder::get( $id_order );
		if ( ! $order || '' === $row['address'] || ! hash_equals( $row['address'], $observation['address'] ) ) {
			return array( 200, array( 'ok' => true, 'ignored' => 'unknown address' ) );
		}

		// Deliveries can arrive out of order. Apply only a newer state_version.
		if ( $observation['state_version'] <= $row['state_version'] ) {
			return array( 200, array( 'ok' => true, 'ignored' => 'stale' ) );
		}

		$state   = (int) $order->current_state;
		$total   = $observation['total_zat'];
		$due     = (int) $row['amount_zat'];
		$zec     = ZcashPaymentUtil::zat_to_zec( $total );
		$due_zec = ZcashPaymentUtil::zat_to_zec( $due );
		$paid    = $g::is_paid( $state );

		$review_note = null;
		if ( $g::hold_state() === $state && ! $row['auto_hold'] ) {
			// A hold the merchant set: leave it for them.
			$review_note = sprintf( 'Zcash total for this order is now %s ZEC. The order is on hold, so it was not completed automatically.', $zec );
		} elseif ( $row['needs_review'] ) {
			// Once funds are set aside for review, keep later receipts there too until the
			// merchant clears the flag, so reconciled or refunded funds never complete the order.
			$review_note = sprintf( 'Zcash total for this order is now %s ZEC. A payment for this order is under manual review, so this was not applied automatically.', $zec );
		} elseif ( ! $g::is_ours( $order ) ) {
			$review_note = sprintf( 'Received %s ZEC at this order\'s abandoned Zcash address, but the order now uses another payment method. Not applied; refund or reconcile by hand.', $zec );
		} elseif ( ! $paid && $row['priced'] !== $g::priced( $order ) ) {
			$review_note = sprintf( 'Received %s ZEC, but the order total changed after the Zcash amount was set. Not completed automatically; reconcile by hand.', $zec );
		}

		if ( null !== $review_note ) {
			$g::note( $id_order, $review_note );
			ZcashPaymentOrder::set( $id_order, array( 'needs_review' => 1 ) );
			if ( $paid && $due > 0 && $total < $due && $g::is_ours( $order ) ) {
				// Review doesn't make an underfunded paid order safe to ship: hold it too.
				if ( ! self::auto_hold( $order, sprintf( 'Zcash funds for this paid order dropped to %s of %s ZEC. Order put on hold; check before shipping.', $zec, $due_zec ) ) ) {
					return array( 500, array( 'error' => 'could not hold order, retry' ) );
				}
			}
		} elseif ( $paid ) {
			if ( $due > 0 && $total < $due ) {
				// A receipt was dropped (expired from the mempool or undone by a reorg).
				$message = sprintf( 'Zcash funds for this paid order dropped to %s of %s ZEC (a transaction was dropped or reorganized out). Order put on hold; check before shipping.', $zec, $due_zec );
				if ( ! self::auto_hold( $order, $message ) ) {
					return array( 500, array( 'error' => 'could not hold order, retry' ) );
				}
			} else {
				$g::note( $id_order, sprintf( 'Zcash total for this order is now %s ZEC.', $zec ) );
			}
		} elseif ( $g::is_closed( $state ) ) {
			// Never revive a closed order automatically: its stock may be gone.
			$g::note( $id_order, sprintf( 'Received %s ZEC for this order after it was closed. Not completed automatically; refund or reinstate the order by hand.', $zec ) );
			ZcashPaymentOrder::set( $id_order, array( 'needs_review' => 1 ) );
		} elseif ( $due > 0 && $total >= $due ) {
			// Deliveries can be retried after a newer state was skipped (for example a receipt
			// that was later dropped). Confirm the service still sees the funds first.
			$live     = $g::service_status( $id_order );
			$live_zat = null === $live ? null : ZcashPaymentBalance::from_status( $live );
			if ( null === $live_zat ) {
				return array( 503, array( 'error' => 'cannot confirm with the service, retry' ) );
			}
			if ( $live_zat < $due ) {
				$g::note( $id_order, sprintf( 'A payment event reported %s ZEC, but the service now sees %s ZEC. Not completed.', $zec, ZcashPaymentUtil::zat_to_zec( $live_zat ) ) );
				return array( 200, array( 'ok' => true, 'ignored' => 'superseded' ) );
			}
			// The status request took time: re-read so a change made meanwhile wins.
			$order = $g::fresh_order( $id_order );
			$row   = ZcashPaymentOrder::get( $id_order );
			$state = $order ? (int) $order->current_state : 0;
			$auto  = $order && $g::hold_state() === $state && $row['auto_hold'];
			if ( ! $order || ! $g::is_ours( $order ) || $row['needs_review'] || $row['priced'] !== $g::priced( $order )
				|| ! ( $g::is_open( $state ) || $auto ) ) {
				// The retry re-evaluates from scratch.
				return array( 503, array( 'error' => 'order changed, retry' ) );
			}
			ZcashPaymentOrder::set( $id_order, array( 'received_zat' => $total ) );
			// Recovering from the module's own hold restores the earlier state; otherwise paid.
			$target = $auto && $row['pre_hold_state'] > 0 ? $row['pre_hold_state'] : (int) Configuration::get( 'PS_OS_PAYMENT' );
			if ( ! $g::set_state( $id_order, $target ) ) {
				// Leave the version unchanged so the service's retry is applied.
				return array( 500, array( 'error' => 'could not complete payment' ) );
			}
			if ( ! $auto ) {
				$g::set_transaction_id( $g::fresh_order( $id_order ) ?? $order, $observation['event_id'] );
				$g::note( $id_order, sprintf( 'Received %s ZEC. Payment complete.', $zec ) );
			} else {
				$g::note( $id_order, sprintf( 'Zcash funds recovered to %s ZEC. Hold released.', $zec ) );
			}
		} else {
			$g::note( $id_order, sprintf( 'Received %s of %s ZEC so far.', $zec, $due_zec ) );
		}

		// Record the version last, after the transition above succeeded.
		ZcashPaymentOrder::set(
			$id_order,
			array(
				'received_zat'  => $total,
				'state_version' => $observation['state_version'],
			)
		);
		return array( 200, array( 'ok' => true ) );
	}

	/**
	 * Puts a paid order on hold, remembering its state so recovery can restore it. Returns false
	 * if the transition failed, so the caller can ask for a retry before recording the event.
	 */
	private static function auto_hold( Order $order, string $message ): bool {
		$g        = ZcashPayment::class;
		$id_order = (int) $order->id;
		$g::note( $id_order, $message );
		if ( $g::hold_state() === (int) $order->current_state ) {
			return true;
		}
		ZcashPaymentOrder::set(
			$id_order,
			array(
				'auto_hold'      => 1,
				'pre_hold_state' => (int) $order->current_state,
			)
		);
		if ( $g::set_state( $id_order, $g::hold_state() ) ) {
			// The status hook clears the hold markers on any change away from the hold state,
			// so they are set before the change and checked after.
			return true;
		}
		// set_state() can fail after the hold was saved (a later hook threw). Keep the markers
		// on an order that is held, so recovery still knows the hold is the module's own.
		$now = $g::fresh_order( $id_order );
		if ( $now && $g::hold_state() === (int) $now->current_state ) {
			return true;
		}
		// Don't leave hold markers on an order that isn't held.
		ZcashPaymentOrder::set(
			$id_order,
			array(
				'auto_hold'      => 0,
				'pre_hold_state' => 0,
			)
		);
		return false;
	}
}
