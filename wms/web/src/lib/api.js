import { get } from 'svelte/store';
import { authStore } from '$lib/stores/authStore';
import { base } from '$app/paths';
import { createAuthRetry } from '$lib/authRetry';

const BASE_URL = '';

function redirectToLogin() {
    if (typeof window !== 'undefined') {
        localStorage.removeItem('token');
        localStorage.removeItem('auth_token');
        localStorage.removeItem('refresh_token');
        window.location.href = `/E/login`;
    }
}

// F15-2: the refresh-and-retry flow lives in authRetry.js (pure, injected
// deps) so it can be unit-tested under plain node; this just wires it to the
// real fetch/store/localStorage.
const authRetry = createAuthRetry({
    fetchFn: (...args) => fetch(...args),
    getRefreshToken: () => (typeof window !== 'undefined' ? localStorage.getItem('refresh_token') : null),
    setTokens: (accessToken, refreshToken, user) => authStore.setTokens(accessToken, refreshToken, user),
    logout: (opts) => authStore.logout(opts),
    redirectToLogin
});

async function request(endpoint, options = {}) {
    const state = get(authStore);
    const headers = {
        'Content-Type': 'application/json',
        ...options.headers
    };

    // Fallback to localStorage if authStore hasn't initialized yet (race with +page.js loaders)
    const token = state.token || (typeof localStorage !== 'undefined' && localStorage.getItem('auth_token'));
    if (token) {
        headers['Authorization'] = `Bearer ${token}`;
    }

    const config = {
        ...options,
        headers
    };

    let response = await fetch(`${BASE_URL}${endpoint}`, config);

    if (response.status === 403) {
        try {
            const data = await response.json();
            if (data.code === 'OBSERVER_FORBIDDEN' && typeof window !== 'undefined') {
                window.dispatchEvent(new CustomEvent('auth:forbidden', { detail: data }));
            }
            throw new Error(data.error || 'Forbidden');
        } catch (e) {
            if (e.message === 'Forbidden') throw e;
            throw new Error('Forbidden');
        }
    }

    if (response.status === 401) {
        // Refresh + retry: only a failed refresh logs out / redirects (F15-2).
        // The retried request's own outcome (AbortError, transport error, an
        // ordinary second 401, ...) propagates/returns unchanged — see
        // authRetry.js.
        response = await authRetry.handleUnauthorized(endpoint, config, BASE_URL);
    }

    return handleResponse(response);
}

async function handleResponse(response) {
    if (!response.ok) {
        const errorData = await response.json().catch(() => ({}));
        throw new Error(errorData.error || `Request failed: ${response.status}`);
    }
    return response.json();
}

export const api = {
    get: (endpoint) => request(endpoint, { method: 'GET' }),
    // `signal` is optional (wave3b / D4) — lets a caller that owns its own
    // AbortController (and a request-generation guard, e.g. the dashboard
    // chat's blocking fallback) cancel the request while still going
    // through this module's 401 refresh/retry and 403 OBSERVER_FORBIDDEN
    // handling, instead of bypassing them with a raw `fetch`.
    post: (endpoint, body, signal) => request(endpoint, { method: 'POST', body: JSON.stringify(body), ...(signal ? { signal } : {}) }),
    put: (endpoint, body) => request(endpoint, { method: 'PUT', body: JSON.stringify(body) }),
    delete: (endpoint) => request(endpoint, { method: 'DELETE' })
};
