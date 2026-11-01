<?php
/**
 * 0.2.0: track the amount seen (mempool included) separately from the confirmed amount.
 */

if ( ! defined( '_PS_VERSION_' ) ) {
	exit;
}

function upgrade_module_0_2_0( $module ) {
	return ZcashPaymentOrder::upgrade();
}
