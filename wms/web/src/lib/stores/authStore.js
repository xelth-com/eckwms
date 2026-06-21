import { writable } from 'svelte/store';
import { browser } from '$app/environment';
import { base } from '$app/paths';

// Simplified auth store based on the snapshot
const initialState = {
  isAuthenticated: false,
  currentUser: null,
  token: null,
  isLoading: true,
  isKioskObserver: false
};

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
                update(s => ({ ...s, isAuthenticated: true, currentUser: user, token, isLoading: false }));
                return;
            } catch {
                localStorage.removeItem('auth_token');
                localStorage.removeItem('refresh_token');
            }
        }
        try {
            const res = await fetch('/api/auth/kiosk-token');
            if (!res.ok) throw new Error('No kiosk token');
            const data = await res.json();
            if (data.success) {
                update(s => ({
                    ...s,
                    isAuthenticated: true,
                    currentUser: data.user,
                    token: data.token,
                    isKioskObserver: true,
                    isLoading: false
                }));
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
            return { success: true };
        } catch (e) {
            update(s => ({ ...s, isLoading: false }));
            return { success: false, error: e.message };
        }
    },
    logout: () => {
        if (browser) {
            localStorage.removeItem('auth_token');
            localStorage.removeItem('refresh_token');
        }
        set({ ...initialState, isLoading: false });
    }
  };
}

export const authStore = createAuthStore();

// Auto-initialize on module load (browser only)
if (browser) {
    authStore.init();
}
