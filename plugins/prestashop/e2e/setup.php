<?php
// Configures the e2e shop: prices the shop in ZEC, and creates a product.
// Install the module first, then run inside the PrestaShop container: php /e2e/setup.php <service-url-for-prestashop>
// Prints the product id on the last line.
require '/var/www/html/config/config.inc.php';

$service_url = $argv[1] ?? 'http://zcash-payment-service:8787';
$module      = Module::getInstanceByName( 'zcashpayment' );
if ( ! $module ) {
	fwrite( STDERR, "zcashpayment module not found\n" );
	exit( 1 );
}
// Install with: php bin/console prestashop:module install zcashpayment (needs the Symfony kernel).
if ( ! Module::isInstalled( 'zcashpayment' ) ) {
	fwrite( STDERR, "zcashpayment module is not installed\n" );
	exit( 1 );
}
Configuration::updateValue( 'ZCASH_SERVICE_URL', $service_url );
Configuration::updateValue( 'ZCASH_WEBHOOK_SECRET', 'e2e-webhook-secret' );
Configuration::updateValue( 'ZCASH_PAY_BASE_URL', '' );
Configuration::updateValue( 'ZCASH_ZEC_RATE', '' );

// The module adds ZEC disabled; make it the only, default currency.
$zec = new Currency( (int) Currency::getIdByIsoCode( 'ZEC', 0, true ) );
if ( ! Validate::isLoadedObject( $zec ) ) {
	fwrite( STDERR, "ZEC currency missing\n" );
	exit( 1 );
}
$zec->active          = true;
$zec->conversion_rate = 1;
$zec->update();
Configuration::updateValue( 'PS_CURRENCY_DEFAULT', (int) $zec->id );
foreach ( Currency::getCurrencies( true, false ) as $c ) {
	if ( (int) $c->id !== (int) $zec->id ) {
		$c->active = false;
		$c->update();
	}
}
// Keep checkout short: no terms checkbox.
Configuration::updateValue( 'PS_CONDITIONS', 0 );

$product                      = new Product();
$product->name                = array( (int) Configuration::get( 'PS_LANG_DEFAULT' ) => 'Test e-book' );
$product->link_rewrite        = array( (int) Configuration::get( 'PS_LANG_DEFAULT' ) => 'test-e-book' );
$product->price               = 0.5;
$product->id_tax_rules_group  = 0;
$product->is_virtual          = true;
$product->active              = true;
$product->visibility          = 'both';
$product->id_category_default = (int) Configuration::get( 'PS_HOME_CATEGORY' );
$product->add();
$product->addToCategories( array( (int) Configuration::get( 'PS_HOME_CATEGORY' ) ) );
StockAvailable::setQuantity( (int) $product->id, 0, 1000 );
Tools::clearAllCache();
echo $product->id, PHP_EOL;
