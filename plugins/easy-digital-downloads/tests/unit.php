<?php
// Plain-PHP unit tests for the dependency-free helpers. Run: php plugins/easy-digital-downloads/tests/unit.php
define( 'ABSPATH', __DIR__ );
require __DIR__ . '/../zcash-payment-edd/includes/class-zcash-edd-util.php';

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

check( 'zec 0.12345678', Zcash_EDD_Util::zec_to_zat( '0.12345678' ), 12345678 );
check( 'zec 1', Zcash_EDD_Util::zec_to_zat( '1' ), 100000000 );
check( 'zec .5', Zcash_EDD_Util::zec_to_zat( '.5' ), 50000000 );
check( 'zec 2.', Zcash_EDD_Util::zec_to_zat( '2.' ), 200000000 );
check( 'zec 9 decimals', Zcash_EDD_Util::zec_to_zat( '0.123456789' ), null );
check( 'zec negative', Zcash_EDD_Util::zec_to_zat( '-1' ), null );
check( 'zec empty', Zcash_EDD_Util::zec_to_zat( '' ), null );
check( 'zec dot', Zcash_EDD_Util::zec_to_zat( '.' ), null );
check( 'zec exponent', Zcash_EDD_Util::zec_to_zat( '1e3' ), null );
check( 'zec huge', Zcash_EDD_Util::zec_to_zat( '99999999999999' ), null );
check( 'edd total 9 decimals', Zcash_EDD_Util::order_total_to_zat( '0.500000000', 'ZEC', '' ), 50000000 );
check( 'edd total sub-zat', Zcash_EDD_Util::order_total_to_zat( '0.123456789', 'ZEC', '' ), null );
check( 'edd total exponent', Zcash_EDD_Util::order_total_to_zat( '5.0E-1', 'ZEC', '' ), 50000000 );
check( 'edd total tiny exponent', Zcash_EDD_Util::order_total_to_zat( '1e-9', 'ZEC', '' ), null );
check( 'edd total 1e-8', Zcash_EDD_Util::order_total_to_zat( '1.0E-8', 'ZEC', '' ), 1 );
check( 'edd total 2e3', Zcash_EDD_Util::order_total_to_zat( '2e3', 'ZEC', '' ), 200000000000 );

check( 'fmt', Zcash_EDD_Util::zat_to_zec( 12345678 ), '0.12345678' );
check( 'fmt whole', Zcash_EDD_Util::zat_to_zec( 250000000 ), '2.50000000' );

check( 'total ZEC store', Zcash_EDD_Util::order_total_to_zat( '1.25', 'ZEC', '' ), 125000000 );
check( 'total USD rate', Zcash_EDD_Util::order_total_to_zat( '40.00', 'USD', '0.025' ), 100000000 );
check( 'total rounds up', Zcash_EDD_Util::order_total_to_zat( '1', 'USD', '0.000000015' ), 2 );
check( 'total no rate', Zcash_EDD_Util::order_total_to_zat( '40.00', 'USD', '' ), null );
check( 'total zero rate', Zcash_EDD_Util::order_total_to_zat( '40.00', 'USD', '0' ), null );

$secret = 'test-secret';
$body   = '{"event_id":"zcash:7:v3","payment_source":"zcash","address":"utest1abc","total_received":"0.50000000","observed_at":"2026-10-31T00:00:00Z"}';
$sig    = hash_hmac( 'sha256', $body, $secret );
check( 'sig ok', Zcash_EDD_Util::verify_signature( $body, $sig, $secret ), true );
check( 'sig uppercase ok', Zcash_EDD_Util::verify_signature( $body, strtoupper( $sig ), $secret ), true );
check( 'sig tampered body', Zcash_EDD_Util::verify_signature( $body . ' ', $sig, $secret ), false );
check( 'sig wrong secret', Zcash_EDD_Util::verify_signature( $body, $sig, 'other' ), false );
check( 'sig missing', Zcash_EDD_Util::verify_signature( $body, null, $secret ), false );
check( 'sig empty secret', Zcash_EDD_Util::verify_signature( $body, hash_hmac( 'sha256', $body, '' ), '' ), false );
check( 'sig malformed', Zcash_EDD_Util::verify_signature( $body, 'zz', $secret ), false );

check(
	'parse',
	Zcash_EDD_Util::parse_observation( $body ),
	array(
		'event_id'       => 'zcash:7:v3',
		'address'        => 'utest1abc',
		'total_received' => '0.50000000',
		'total_zat'      => 50000000,
		'state_version'  => 3,
	)
);
check( 'parse wrong source', Zcash_EDD_Util::parse_observation( str_replace( '"zcash"', '"btc"', $body ) ), null );
check( 'parse no version', Zcash_EDD_Util::parse_observation( str_replace( ':v3', '', $body ) ), null );
check( 'parse not json', Zcash_EDD_Util::parse_observation( 'nope' ), null );

check( 'pay url absolute', Zcash_EDD_Util::resolve_pay_url( 'https://pay.example/pay/abc', '' ), 'https://pay.example/pay/abc' );
check( 'pay url relative + base', Zcash_EDD_Util::resolve_pay_url( '/pay/abc', 'https://pay.example/' ), 'https://pay.example/pay/abc' );
check( 'pay url relative no base', Zcash_EDD_Util::resolve_pay_url( '/pay/abc', '' ), null );
check( 'pay url empty', Zcash_EDD_Util::resolve_pay_url( '', 'https://pay.example' ), null );
check( 'pay url bad base', Zcash_EDD_Util::resolve_pay_url( '/pay/abc', 'javascript:alert(1)' ), null );

if ( $failures > 0 ) {
	echo "$failures failure(s)\n";
	exit( 1 );
}
echo "all passed\n";
