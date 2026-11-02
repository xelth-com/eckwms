import adapter from '@sveltejs/adapter-static';

/** @type {import('@sveltejs/kit').Config} */
const config = {
	kit: {
		// Короткое имя для минимизации размера URL и QR кодов
		appDir: 'i',

		adapter: adapter({
			fallback: 'index.html',
			strict: false
		}),
		paths: {
			// Базовый путь для продакшена
			base: '/E'
		},
		// Poll the deployed build's version every 60s so `$updated` from
		// `$app/stores` flips true after an OTA deploy — the root layout
		// uses it to reload the kiosk browser once the user goes idle
		// (see src/routes/+layout.svelte).
		version: {
			pollInterval: 60000
		}
	}
};

export default config;
