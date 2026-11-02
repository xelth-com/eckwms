// Pure refresh-and-retry flow for a 401 response, extracted out of api.js so
// it can be unit-tested under plain node (api.js itself pulls in
// `$app/paths` / `$lib/stores/authStore`, which only resolve inside
// SvelteKit). All side effects are injected.
//
// F15-2: a FAILED REFRESH (the refresh call rejects or returns a non-ok
// status) is the only thing that logs the user out and redirects to login.
// Once the refresh has succeeded, the new credentials stay installed no
// matter what happens to the retried request — an aborted retry, a plain
// transport error, or an ordinary second 401 are the caller's problem, not a
// reason to tear down the session. `handleUnauthorized` never catches the
// retry's own fetch, so whatever it throws (e.g. `AbortError`) propagates to
// the caller unchanged, and a second-401 *response* just comes back as an
// ordinary response for the caller to handle — no second refresh loop.

export function createAuthRetry({ fetchFn, getRefreshToken, setTokens, logout, redirectToLogin }) {
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

    // Called when `endpoint`'s request (described by `originalRequestConfig`)
    // came back 401. Returns the retried fetch Response (ok or not — the
    // caller runs it through its normal response handling), or throws:
    // - the retry's own error (AbortError, transport error, ...), unchanged
    // - a refresh-failure error, after logout()+redirectToLogin() have run
    async function handleUnauthorized(endpoint, originalRequestConfig, baseUrl = '') {
        // A refresh is already in flight for another request: queue behind
        // it and retry with whatever token that refresh installs.
        if (isRefreshing) {
            const token = await new Promise((resolve, reject) => {
                failedQueue.push({ resolve, reject });
            });
            originalRequestConfig.headers['Authorization'] = `Bearer ${token}`;
            return fetchFn(`${baseUrl}${endpoint}`, originalRequestConfig);
        }

        const refreshToken = getRefreshToken();

        if (!refreshToken) {
            // Session-expiry path, not a user action (D5 / F08-9): the same
            // user is expected to sign back in, so their saved chat
            // conversation must survive this.
            logout({ preserveChat: true });
            redirectToLogin();
            throw new Error('Unauthorized');
        }

        isRefreshing = true;
        let accessToken;
        try {
            const refreshRes = await fetchFn(`${baseUrl}/auth/refresh`, {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ refreshToken })
            });

            if (!refreshRes.ok) {
                throw new Error('Refresh token invalid or expired');
            }

            const data = await refreshRes.json();
            accessToken = data.tokens.accessToken;
            setTokens(data.tokens.accessToken, data.tokens.refreshToken, data.user);
        } catch (err) {
            isRefreshing = false;
            processQueue(err, null);
            // Same reasoning as above: a failed refresh is a session
            // expiry, not a user-initiated logout.
            logout({ preserveChat: true });
            redirectToLogin();
            throw new Error('Session expired');
        }

        // Refresh succeeded: the new credentials are installed and every
        // queued request gets them, regardless of what the retry below does.
        isRefreshing = false;
        processQueue(null, accessToken);

        originalRequestConfig.headers['Authorization'] = `Bearer ${accessToken}`;
        // Intentionally NOT wrapped in try/catch: an AbortError (or any other
        // failure) of this retried request must propagate to the caller as
        // itself, not be reinterpreted as a refresh failure.
        return fetchFn(`${baseUrl}${endpoint}`, originalRequestConfig);
    }

    return { handleUnauthorized };
}
