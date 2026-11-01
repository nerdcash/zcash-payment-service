<?php
// Plain-PHP unit tests for the dependency-free helpers. Run: php plugins/prestashop/tests/unit.php
define( '_PS_VERSION_', 'test' );
require __DIR__ . '/../zcashpayment/classes/ZcashPaymentUtil.php';
require __DIR__ . '/../zcashpayment/classes/ZcashPaymentBalance.php';

$failures = 0;
function check( string $name, $actual, $expected ): void {
	global $failures;
	if ( $actual === $expected ) {
		echo "ok   $name\n";
	} else {
		$failures++;
		echo "FAIL $name: expected " . var_export( $expected, true ) . ', got ' . var_export( $actual, true ) . "\n";
	}
}

check( 'zec 0.12345678', ZcashPaymentUtil::zec_to_zat( '0.12345678' ), 12345678 );
check( 'zec 1', ZcashPaymentUtil::zec_to_zat( '1' ), 100000000 );
check( 'zec .5', ZcashPaymentUtil::zec_to_zat( '.5' ), 50000000 );
check( 'zec 2.', ZcashPaymentUtil::zec_to_zat( '2.' ), 200000000 );
check( 'zec 9 decimals', ZcashPaymentUtil::zec_to_zat( '0.123456789' ), null );
check( 'zec negative', ZcashPaymentUtil::zec_to_zat( '-1' ), null );
check( 'zec empty', ZcashPaymentUtil::zec_to_zat( '' ), null );
check( 'zec dot', ZcashPaymentUtil::zec_to_zat( '.' ), null );
check( 'zec exponent', ZcashPaymentUtil::zec_to_zat( '1e3' ), null );
check( 'zec huge', ZcashPaymentUtil::zec_to_zat( '99999999999999' ), null );
check( 'total 9 decimals', ZcashPaymentUtil::order_total_to_zat( '0.500000000', 'ZEC', '' ), 50000000 );
check( 'total sub-zat', ZcashPaymentUtil::order_total_to_zat( '0.123456789', 'ZEC', '' ), null );
check( 'total exponent', ZcashPaymentUtil::order_total_to_zat( '5.0E-1', 'ZEC', '' ), 50000000 );
check( 'total tiny exponent', ZcashPaymentUtil::order_total_to_zat( '1e-9', 'ZEC', '' ), null );
check( 'total 1e-8', ZcashPaymentUtil::order_total_to_zat( '1.0E-8', 'ZEC', '' ), 1 );
check( 'total 2e3', ZcashPaymentUtil::order_total_to_zat( '2e3', 'ZEC', '' ), 200000000000 );

check( 'fmt', ZcashPaymentUtil::zat_to_zec( 12345678 ), '0.12345678' );
check( 'fmt whole', ZcashPaymentUtil::zat_to_zec( 250000000 ), '2.50000000' );

check( 'total ZEC store', ZcashPaymentUtil::order_total_to_zat( '1.25', 'ZEC', '' ), 125000000 );
check( 'total USD rate', ZcashPaymentUtil::order_total_to_zat( '40.00', 'USD', '0.025' ), 100000000 );
check( 'total rounds up', ZcashPaymentUtil::order_total_to_zat( '1', 'USD', '0.000000015' ), 2 );
check( 'total no rate', ZcashPaymentUtil::order_total_to_zat( '40.00', 'USD', '' ), null );
check( 'total zero rate', ZcashPaymentUtil::order_total_to_zat( '40.00', 'USD', '0' ), null );

$secret = 'test-secret';
$body   = '{"event_id":"zcash:7:v3","payment_source":"zcash","address":"utest1abc","total_received":"0.50000000","observed_at":"2026-10-31T00:00:00Z"}';
$sig    = hash_hmac( 'sha256', $body, $secret );
check( 'sig ok', ZcashPaymentUtil::verify_signature( $body, $sig, $secret ), true );
check( 'sig uppercase ok', ZcashPaymentUtil::verify_signature( $body, strtoupper( $sig ), $secret ), true );
check( 'sig tampered body', ZcashPaymentUtil::verify_signature( $body . ' ', $sig, $secret ), false );
check( 'sig wrong secret', ZcashPaymentUtil::verify_signature( $body, $sig, 'other' ), false );
check( 'sig missing', ZcashPaymentUtil::verify_signature( $body, null, $secret ), false );
check( 'sig empty secret', ZcashPaymentUtil::verify_signature( $body, hash_hmac( 'sha256', $body, '' ), '' ), false );
check( 'sig malformed', ZcashPaymentUtil::verify_signature( $body, 'zz', $secret ), false );

check(
	'parse',
	ZcashPaymentUtil::parse_observation( $body ),
	array(
		'event_id'       => 'zcash:7:v3',
		'address'        => 'utest1abc',
		'total_received' => '0.50000000',
		'total_zat'      => 50000000,
		'state_version'  => 3,
	)
);
check( 'parse wrong source', ZcashPaymentUtil::parse_observation( str_replace( '"zcash"', '"btc"', $body ) ), null );
check( 'parse no version', ZcashPaymentUtil::parse_observation( str_replace( ':v3', '', $body ) ), null );
check( 'parse not json', ZcashPaymentUtil::parse_observation( 'nope' ), null );

check( 'pay url absolute', ZcashPaymentUtil::resolve_pay_url( 'https://pay.example/pay/abc', '' ), 'https://pay.example/pay/abc' );
check( 'pay url relative + base', ZcashPaymentUtil::resolve_pay_url( '/pay/abc', 'https://pay.example/' ), 'https://pay.example/pay/abc' );
check( 'pay url relative no base', ZcashPaymentUtil::resolve_pay_url( '/pay/abc', '' ), null );
check( 'pay url empty', ZcashPaymentUtil::resolve_pay_url( '', 'https://pay.example' ), null );
check( 'pay url bad base', ZcashPaymentUtil::resolve_pay_url( '/pay/abc', 'javascript:alert(1)' ), null );

check( 'balance webhook', ZcashPaymentBalance::from_webhook( array( 'total_received' => '1.5' ) ), 150000000 );
check( 'balance webhook missing', ZcashPaymentBalance::from_webhook( array() ), null );
check( 'balance webhook not string', ZcashPaymentBalance::from_webhook( array( 'total_received' => 1 ) ), null );
check( 'balance status', ZcashPaymentBalance::from_status( array( 'total_received' => '0.00000001' ) ), 1 );
check( 'total PrestaShop 6 decimals', ZcashPaymentUtil::order_total_to_zat( '0.500000', 'ZEC', '' ), 50000000 );

if ( $failures > 0 ) {
	echo "$failures failure(s)\n";
	exit( 1 );
}
echo "all passed\n";
