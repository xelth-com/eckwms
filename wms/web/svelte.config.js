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
		}
	}
};

export default config;
