import { get } from 'svelte/store';
import { authStore } from '$lib/stores/authStore';
import { base } from '$app/paths';

const BASE_URL = '';

let isRefreshing = false;
let failedQueue = [];

const processQueue = (error, token = null) => {
    failedQueue.forEach(prom => {
        if (error) {
            prom.reject(error);
        } else {
            prom.resolve(token);
        }
    });
    failedQueue = [];
};

function redirectToLogin() {
    if (typeof window !== 'undefined') {
        localStorage.removeItem('token');
        localStorage.removeItem('auth_token');
        localStorage.removeItem('refresh_token');
        window.location.href = `/E/login`;
    }
}

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
        const originalRequestConfig = config;

        // If a refresh is already in progress, queue this request
        if (isRefreshing) {
            return new Promise(function (resolve, reject) {
                failedQueue.push({ resolve, reject });
            })
            .then(token => {
                originalRequestConfig.headers['Authorization'] = `Bearer ${token}`;
                return fetch(`${BASE_URL}${endpoint}`, originalRequestConfig).then(handleResponse);
            })
            .catch(err => {
                throw err;
            });
        }

        const refreshToken = typeof window !== 'undefined' ? localStorage.getItem('refresh_token') : null;

        if (!refreshToken) {
            authStore.logout();
            redirectToLogin();
            throw new Error('Unauthorized');
        }

        isRefreshing = true;

        try {
            const refreshRes = await fetch(`${BASE_URL}/auth/refresh`, {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ refreshToken })
            });

            if (!refreshRes.ok) {
                throw new Error('Refresh token invalid or expired');
            }

            const data = await refreshRes.json();

            authStore.setTokens(data.tokens.accessToken, data.tokens.refreshToken, data.user);

            processQueue(null, data.tokens.accessToken);

            // Retry the original request
            originalRequestConfig.headers['Authorization'] = `Bearer ${data.tokens.accessToken}`;
            response = await fetch(`${BASE_URL}${endpoint}`, originalRequestConfig);

        } catch (err) {
            processQueue(err, null);
            authStore.logout();
            redirectToLogin();
            throw new Error('Session expired');
        } finally {
            isRefreshing = false;
        }
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
    post: (endpoint, body) => request(endpoint, { method: 'POST', body: JSON.stringify(body) }),
    put: (endpoint, body) => request(endpoint, { method: 'PUT', body: JSON.stringify(body) }),
    delete: (endpoint) => request(endpoint, { method: 'DELETE' })
};
