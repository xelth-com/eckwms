import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createAuthRetry } from './authRetry.js';

// F15-2: an AbortError on the RETRIED request (after a successful refresh)
// must reach the caller as AbortError, must not log the user out, and must
// not redirect to login. Only a failed refresh does that.

function makeDeps({ fetchFn }) {
    const setTokensCalls = [];
    const logoutCalls = [];
    const redirectCalls = [];
    const deps = {
        fetchFn,
        getRefreshToken: () => 'refresh-token-abc',
        setTokens: (accessToken, refreshToken, user) => setTokensCalls.push({ accessToken, refreshToken, user }),
        logout: (opts) => logoutCalls.push(opts),
        redirectToLogin: () => redirectCalls.push(true)
    };
    return { deps, setTokensCalls, logoutCalls, redirectCalls };
}

test('F15-2: refresh ok, retry aborted -> caller gets AbortError, tokens set once, no logout, no redirect', async () => {
    let refreshCalls = 0;
    let retryCalls = 0;
    const fetchFn = async (url) => {
        if (url.includes('/auth/refresh')) {
            refreshCalls++;
            return {
                ok: true,
                json: async () => ({
                    tokens: { accessToken: 'new-access', refreshToken: 'new-refresh' },
                    user: { id: 1 }
                })
            };
        }
        retryCalls++;
        const err = new Error('The operation was aborted.');
        err.name = 'AbortError';
        throw err;
    };

    const { deps, setTokensCalls, logoutCalls, redirectCalls } = makeDeps({ fetchFn });
    const authRetry = createAuthRetry(deps);

    await assert.rejects(
        () => authRetry.handleUnauthorized('/api/ai/chat', { headers: {} }, ''),
        (err) => err.name === 'AbortError'
    );

    assert.equal(refreshCalls, 1);
    assert.equal(retryCalls, 1);
    assert.equal(setTokensCalls.length, 1);
    assert.deepEqual(setTokensCalls[0], { accessToken: 'new-access', refreshToken: 'new-refresh', user: { id: 1 } });
    assert.equal(logoutCalls.length, 0);
    assert.equal(redirectCalls.length, 0);
});

test('F15-2: refresh fails -> logout + redirect (old behaviour preserved)', async () => {
    let refreshCalls = 0;
    const fetchFn = async (url) => {
        if (url.includes('/auth/refresh')) {
            refreshCalls++;
            return { ok: false, json: async () => ({}) };
        }
        throw new Error('should not retry the original request after a failed refresh');
    };

    const { deps, setTokensCalls, logoutCalls, redirectCalls } = makeDeps({ fetchFn });
    const authRetry = createAuthRetry(deps);

    await assert.rejects(
        () => authRetry.handleUnauthorized('/api/ai/chat', { headers: {} }, ''),
        /Session expired/
    );

    assert.equal(refreshCalls, 1);
    assert.equal(setTokensCalls.length, 0);
    assert.equal(logoutCalls.length, 1);
    assert.deepEqual(logoutCalls[0], { preserveChat: true });
    assert.equal(redirectCalls.length, 1);
});

test('refresh ok, retry ok -> the retried response is returned', async () => {
    let refreshCalls = 0;
    let retryCalls = 0;
    const retriedResponse = { ok: true, status: 200, json: async () => ({ answer: 'hi' }) };
    const fetchFn = async (url) => {
        if (url.includes('/auth/refresh')) {
            refreshCalls++;
            return {
                ok: true,
                json: async () => ({
                    tokens: { accessToken: 'new-access', refreshToken: 'new-refresh' },
                    user: { id: 1 }
                })
            };
        }
        retryCalls++;
        return retriedResponse;
    };

    const { deps, setTokensCalls, logoutCalls, redirectCalls } = makeDeps({ fetchFn });
    const authRetry = createAuthRetry(deps);

    const config = { headers: {} };
    const result = await authRetry.handleUnauthorized('/api/ai/chat', config, '');

    assert.equal(result, retriedResponse);
    assert.equal(refreshCalls, 1);
    assert.equal(retryCalls, 1);
    assert.equal(config.headers['Authorization'], 'Bearer new-access');
    assert.equal(setTokensCalls.length, 1);
    assert.equal(logoutCalls.length, 0);
    assert.equal(redirectCalls.length, 0);
});

test('refresh ok, retry itself returns an ordinary second 401 -> returned as-is, no second refresh loop', async () => {
    let refreshCalls = 0;
    const secondUnauthorized = { ok: false, status: 401, json: async () => ({ error: 'still unauthorized' }) };
    const fetchFn = async (url) => {
        if (url.includes('/auth/refresh')) {
            refreshCalls++;
            return {
                ok: true,
                json: async () => ({
                    tokens: { accessToken: 'new-access', refreshToken: 'new-refresh' },
                    user: { id: 1 }
                })
            };
        }
        return secondUnauthorized;
    };

    const { deps, logoutCalls, redirectCalls } = makeDeps({ fetchFn });
    const authRetry = createAuthRetry(deps);

    const result = await authRetry.handleUnauthorized('/api/ai/chat', { headers: {} }, '');

    assert.equal(result, secondUnauthorized);
    assert.equal(refreshCalls, 1); // no second refresh attempt
    assert.equal(logoutCalls.length, 0);
    assert.equal(redirectCalls.length, 0);
});

test('no refresh token available -> logout + redirect without calling fetch', async () => {
    const fetchFn = async () => {
        throw new Error('fetch should not be called when there is no refresh token');
    };
    const { deps, logoutCalls, redirectCalls } = makeDeps({ fetchFn });
    deps.getRefreshToken = () => null;
    const authRetry = createAuthRetry(deps);

    await assert.rejects(() => authRetry.handleUnauthorized('/api/ai/chat', { headers: {} }, ''), /Unauthorized/);

    assert.equal(logoutCalls.length, 1);
    assert.deepEqual(logoutCalls[0], { preserveChat: true });
    assert.equal(redirectCalls.length, 1);
});
