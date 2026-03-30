/** @type {import('@sveltejs/kit').Handle} */
export async function handle({ event, resolve }) {
	const { pathname } = event.url;

	// Proxy API and auth requests to Go backend
	if (pathname.startsWith('/api/') || pathname.startsWith('/auth/') || pathname === '/health') {
		const backendUrl = `http://localhost:3210${pathname}${event.url.search}`;

		const headers = new Headers();
		for (const [key, value] of event.request.headers) {
			headers.set(key, value);
		}

		const response = await fetch(backendUrl, {
			method: event.request.method,
			headers,
			body: event.request.method !== 'GET' && event.request.method !== 'HEAD'
				? await event.request.text()
				: undefined
		});

		const responseHeaders = new Headers();
		for (const [key, value] of response.headers) {
			responseHeaders.set(key, value);
		}

		return new Response(response.body, {
			status: response.status,
			statusText: response.statusText,
			headers: responseHeaders
		});
	}

	return resolve(event);
}
