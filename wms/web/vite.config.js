import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

export default defineConfig({
	plugins: [sveltekit()],
	// Local dev loop: serve the SPA from Vite (instant HMR) but send API/auth/ws
	// to the running WMS backend on :3210. Use a fast RELEASE wms so dashboard
	// queries don't crawl (a debug build makes /api take ~30s). Dev-only; the
	// production build is still embedded into the WMS via rust-embed.
	server: {
		proxy: {
			'/api': 'http://localhost:3210',
			'/auth': 'http://localhost:3210',
			'/E/ws': { target: 'ws://localhost:3210', ws: true }
		}
	}
});
