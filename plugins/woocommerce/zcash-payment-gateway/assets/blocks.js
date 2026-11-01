/* Registers the Zcash gateway with the WooCommerce checkout block. No build step needed. */
(function () {
	const { registerPaymentMethod } = window.wc.wcBlocksRegistry;
	const { getSetting } = window.wc.wcSettings;
	const { createElement } = window.wp.element;
	const { decodeEntities } = window.wp.htmlEntities;

	const settings = getSetting('zcash_payment_data', {});
	const title = decodeEntities(settings.title || 'Zcash (ZEC)');
	const Content = () => createElement('p', null, decodeEntities(settings.description || ''));

	registerPaymentMethod({
		name: 'zcash_payment',
		label: createElement('span', null, title),
		ariaLabel: title,
		content: createElement(Content),
		edit: createElement(Content),
		canMakePayment: () => true,
		supports: { features: settings.supports || ['products'] },
	});
})();
