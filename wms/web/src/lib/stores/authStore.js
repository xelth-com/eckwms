import { writable } from 'svelte/store';
import { browser } from '$app/environment';
import { base } from '$app/paths';
import { applyUserLocale } from '$lib/i18n';
import { usernameOf, clearChatConversationFor } from '$lib/chatStorage';

// Simplified auth store based on the snapshot
const initialState = {
  isAuthenticated: false,
  currentUser: null,
  token: null,
  isLoading: true,
  isKioskObserver: false,
  // { available: true, delaySecs } once kiosk-token says auto-login is
  // configured but delayed (delay_secs > 0) — the login page counts this
  // down instead of signing in immediately. null = not available / unknown.
  kioskAutoLogin: null
};

const KIOSK_SCREEN_FLAG = 'eck_kiosk_screen';

// "This browser is the node's own kiosk-class screen" — remembered in
// localStorage so the in-app input method (lib/ime) can default to the alternate
// layout there while staying off on office PCs. isKioskObserver is not enough:
// it flips to false once a real user logs in.
function setKioskScreenFlag() {
    try {
        if (localStorage.getItem(KIOSK_SCREEN_FLAG) === '1') return;
        localStorage.setItem(KIOSK_SCREEN_FLAG, '1');
    } catch {
        return;
    }
    // The IME may have computed its default before we knew — let it re-apply.
    window.dispatchEvent(new CustomEvent('eck:kiosk-screen'));
}

// Kiosk auto-login answered. On public-exhibition nodes (ECK_PUBLIC_OBSERVER=1)
// the server answers EVERY remote visitor with {success:true, mode:"wms",
// user:{id:"gast"}} — those are NOT kiosk screens. Only the real loopback kiosk
// is: it replies with mode:"pos" (register, no user) or user.id === "kiosk".
function markKioskScreen(data) {
    if (!browser || !data) return;
    if (!(data.mode === 'pos' || data.user?.id === 'kiosk')) return;
    setKioskScreenFlag();
}

function isLoopbackHost() {
    const h = window.location.hostname;
    return h === '127.0.0.1' || h === 'localhost' || h === '[::1]' || h === '::1';
}

// The page is served from a loopback origin = the browser runs on the node itself.
//  - Linux: a kiosk-class device (cage + Chromium, one fixed XKB layout, no OS
//    IME) whether or not kiosk auto-login is enabled — the pilot kiosk runs with
//    kiosk mode OFF and a signed-in user, so kiosk-token alone never marked it.
//  - any other OS on loopback is a developer/office box with its own OS layouts:
//    a kiosk screen only if the server says kiosk mode is on. A signed-in session
//    never asks for a kiosk token, so ask once in the background (the endpoint is
//    loopback-only; the returned token is ignored). Signed out, init() asks anyway.
function probeKioskScreen(signedIn) {
    if (!browser || !isLoopbackHost()) return;
    try {
        if (localStorage.getItem(KIOSK_SCREEN_FLAG) === '1') return;
    } catch {
        return;
    }
    const ua = navigator.userAgent || '';
    if (/Linux/i.test(ua) && !/Android/i.test(ua)) {
        setKioskScreenFlag();
        return;
    }
    if (!signedIn) return;
    fetch('/api/auth/kiosk-token')
        .then((r) => (r.ok ? r.json() : null))
        .then((d) => markKioskScreen(d))
        .catch(() => {});
}

function createAuthStore() {
  const { subscribe, set, update } = writable(initialState);

  return {
    subscribe,
    init: async () => {
        if (!browser) return;
        const token = localStorage.getItem('auth_token');
        if (token) {
            try {
                const res = await fetch('/api/auth/me', {
                    headers: { 'Authorization': `Bearer ${token}` }
                });
                if (!res.ok) throw new Error('Token invalid');
                const user = await res.json();
                // /me historically exposes the id as user_id; the login payload
                // uses id. Normalize so consumers (locale override matching,
                // per-user persistence) see one shape.
                if (user && user.id == null && user.user_id != null) user.id = user.user_id;
                update(s => ({ ...s, isAuthenticated: true, currentUser: user, token, isLoading: false }));
                applyUserLocale(user);
                probeKioskScreen(true);
                return;
            } catch {
                localStorage.removeItem('auth_token');
                localStorage.removeItem('refresh_token');
            }
        }
        probeKioskScreen(false);
        try {
            const res = await fetch('/api/auth/kiosk-token');
            if (!res.ok) throw new Error('No kiosk token');
            const data = await res.json();
            markKioskScreen(data);
            // Kiosk boot mode "pos": this device is a register — no observer
            // token is minted, the shell goes straight to the POS PIN pad.
            if (data.success && data.mode === 'pos' && !data.token) {
                window.location.replace('/K/');
                return;
            }
            // Auto-login is configured but delayed: stay on the login page
            // and let it count down (login/+page.svelte onMount). Signing
            // in immediately here would skip the countdown entirely — the
            // whole point is giving a real operator a window to use the
            // form before the kiosk observer takes over.
            if (data.success && data.mode !== 'pos' && (data.delay_secs ?? 0) > 0) {
                update(s => ({ ...s, kioskAutoLogin: { available: true, delaySecs: data.delay_secs }, isLoading: false }));
                return;
            }
            if (data.success) {
                update(s => ({
                    ...s,
                    isAuthenticated: true,
                    currentUser: data.user,
                    token: data.token,
                    isKioskObserver: true,
                    isLoading: false
                }));
                applyUserLocale(data.user);
                return;
            }
        } catch {}
        update(s => ({ ...s, isLoading: false }));
    },
    setTokens: (accessToken, refreshToken, user) => {
        if (browser) {
            localStorage.setItem('auth_token', accessToken);
            if (refreshToken) {
                localStorage.setItem('refresh_token', refreshToken);
            }
        }
        update(s => ({
            ...s,
            isAuthenticated: true,
            currentUser: user || s.currentUser,
            token: accessToken,
            isLoading: false
        }));
        if (user) applyUserLocale(user);
    },
    login: async (username, password) => {
        update(s => ({ ...s, isLoading: true }));
        try {
            const res = await fetch('/api/auth/login', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ username, password })
            });

            const data = await res.json();
            if (!data.success) throw new Error(data.error || 'Login failed');

            if (browser) {
                localStorage.setItem('auth_token', data.token);
            }

            update(s => ({
                ...s,
                isAuthenticated: true,
                currentUser: data.user,
                token: data.token,
                isKioskObserver: false,
                isLoading: false
            }));
            applyUserLocale(data.user);
            return { success: true };
        } catch (e) {
            update(s => ({ ...s, isLoading: false }));
            return { success: false, error: e.message };
        }
    },
    // `logout()` (no args) is the UI's actual "Log out" action — on a
    // shared kiosk browser the next account must not be able to restore
    // this user's Brain Chat conversation, so it drops it too (engine
    // preference kept — it's a UI setting, not conversation data).
    //
    // Re-ask the server whether kiosk auto-login is configured (delayed or
    // immediate). init() only asks once at module load, so this lets the
    // login page learn the current answer whenever IT mounts fresh — in
    // particular right after a logout, when init() already ran with the
    // previous user's token and never took this branch. Loopback-only like
    // the rest of the kiosk probing (never phones remote browsers).
    probeKioskAutoLogin: async () => {
        if (!browser || !isLoopbackHost()) return { available: false, delaySecs: 0 };
        try {
            const res = await fetch('/api/auth/kiosk-token');
            if (!res.ok) throw new Error('No kiosk token');
            const data = await res.json();
            if (data.success && data.mode !== 'pos' && (data.delay_secs ?? 0) > 0) {
                update(s => ({ ...s, kioskAutoLogin: { available: true, delaySecs: data.delay_secs } }));
                return { available: true, delaySecs: data.delay_secs };
            }
            update(s => ({ ...s, kioskAutoLogin: null }));
            return { available: false, delaySecs: 0 };
        } catch {
            return { available: false, delaySecs: 0 };
        }
    },
    // The countdown hit zero (or `?real=1` was absent and delay was 0):
    // mint and apply the kiosk observer token, same as init()'s immediate
    // branch.
    kioskAutoLogin: async () => {
        if (!browser) return false;
        try {
            const res = await fetch('/api/auth/kiosk-token');
            if (!res.ok) throw new Error('No kiosk token');
            const data = await res.json();
            markKioskScreen(data);
            if (data.success && data.mode === 'pos' && !data.token) {
                window.location.replace('/K/');
                return true;
            }
            if (data.success && data.token) {
                update(s => ({
                    ...s,
                    isAuthenticated: true,
                    currentUser: data.user,
                    token: data.token,
                    isKioskObserver: true,
                    isLoading: false
                }));
                applyUserLocale(data.user);
                return true;
            }
            return false;
        } catch {
            return false;
        }
    },
    // wave3b / F08-9: `api.js` also calls this internally on a session
    // expiry it cannot recover from (401 with no refresh token, or a
    // failed refresh) — NOT a user action, and the same user is expected
    // to sign back in, so that path must not delete their saved
    // conversation. Those call sites pass `{ preserveChat: true }`.
    logout: (opts = {}) => {
        if (browser) {
            localStorage.removeItem('auth_token');
            localStorage.removeItem('refresh_token');
            if (!opts.preserveChat) {
                let loggedOutUser = null;
                const unsubscribe = subscribe((s) => { loggedOutUser = s.currentUser; });
                unsubscribe();
                clearChatConversationFor(usernameOf(loggedOutUser));
            }
        }
        set({ ...initialState, isLoading: false });
    },
    // Clear the forced-password-change flag locally after a successful change,
    // so the blocking modal unlocks without needing a re-login.
    clearMustChangePassword: () => {
        update(s => s.currentUser
            ? { ...s, currentUser: { ...s.currentUser, mustChangePassword: false } }
            : s);
    }
  };
}

export const authStore = createAuthStore();

// Auto-initialize on module load (browser only)
if (browser) {
    authStore.init();
}
