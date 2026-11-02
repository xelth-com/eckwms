import { writable } from 'svelte/store';
import { browser } from '$app/environment';

const RECONNECT_INTERVAL = 3000;

function createWsStore() {
    const { subscribe, set, update } = writable({
        connected: false,
        lastMessage: null,
        error: null
    });

    let socket;
    let reconnectTimer;
    let explicitClose = false;

    // Stable per-browser screen identity (scan-mirror target, §5). Created once
    // and persisted; the name defaults to "<hostname or Kiosk> · <user>" and is
    // operator-editable from the dashboard header.
    function getScreenId() {
        if (!browser) return null;
        try {
            let id = localStorage.getItem('eck_screen_id');
            if (!id) {
                id = (crypto?.randomUUID?.() ||
                    ('s-' + Date.now().toString(36) + '-' + Math.random().toString(36).slice(2)));
                localStorage.setItem('eck_screen_id', id);
            }
            return id;
        } catch {
            return null;
        }
    }

    function defaultScreenName() {
        let host = 'Kiosk';
        try { host = window.location.hostname || 'Kiosk'; } catch {}
        let user = '';
        try { user = localStorage.getItem('eck_screen_user') || ''; } catch {}
        return user ? `${host} · ${user}` : host;
    }

    function getScreenName() {
        if (!browser) return null;
        try {
            return localStorage.getItem('eck_screen_name') || defaultScreenName();
        } catch {
            return defaultScreenName();
        }
    }

    function connect() {
        if (!browser) return;
        if (socket && (socket.readyState === WebSocket.CONNECTING || socket.readyState === WebSocket.OPEN)) return;

        // Determine protocol (ws or wss) and construct URL
        const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
        const host = window.location.host;
        // Use /E/ws path (HTTP_PATH_PREFIX=/E from backend config)
        // CaseInsensitiveMiddleware on backend handles both /E/ws and /e/ws
        // JWT goes as a query param — the browser WebSocket API can't set an
        // Authorization header, and the server only streams broadcast events
        // (TRIP_LIVE etc.) to authenticated sockets.
        // screen_id/screen_name register this browser as a PDA scan-mirror
        // target (see GET /api/screens).
        const token = localStorage.getItem('auth_token');
        const params = new URLSearchParams();
        if (token) params.set('token', token);
        const screenId = getScreenId();
        if (screenId) {
            params.set('screen_id', screenId);
            params.set('screen_name', getScreenName());
        }
        const qs = params.toString();
        const url = `${protocol}//${host}/E/ws${qs ? `?${qs}` : ''}`;

        console.log(`[WS] Connecting to ${url}...`);
        socket = new WebSocket(url);

        socket.onopen = () => {
            console.log('[WS] Connected');
            update(s => ({ ...s, connected: true, error: null }));
        };

        socket.onclose = () => {
            console.log('[WS] Disconnected');
            update(s => ({ ...s, connected: false }));

            if (!explicitClose) {
                clearTimeout(reconnectTimer);
                reconnectTimer = setTimeout(connect, RECONNECT_INTERVAL);
            }
        };

        socket.onerror = (e) => {
            console.error('[WS] Connection Error', e);
            update(s => ({ ...s, error: 'Connection error' }));
        };

        socket.onmessage = (event) => {
            try {
                const data = JSON.parse(event.data);
                // Update timestamp to ensure svelte reactive statements trigger even if data is same
                data._receivedAt = Date.now();
                update(s => ({ ...s, lastMessage: data }));
            } catch (err) {
                console.error('[WS] Failed to parse message', err);
            }
        };
    }

    function close() {
        explicitClose = true;
        if (socket) socket.close();
    }

    // Force a fresh connection (e.g. after a screen rename, so the server's
    // live screen set picks up the new name). Closes the current socket and
    // lets onclose/explicitClose logic reconnect immediately.
    function reconnect() {
        if (!browser) return;
        explicitClose = false;
        clearTimeout(reconnectTimer);
        if (socket && (socket.readyState === WebSocket.OPEN || socket.readyState === WebSocket.CONNECTING)) {
            try { socket.close(); } catch {}
            // onclose schedules a reconnect; nudge it to run now instead of after 3 s.
            reconnectTimer = setTimeout(connect, 150);
        } else {
            connect();
        }
    }

    // Persisted screen name (scan-mirror target label). Setting it stores the
    // override and reconnects so the server re-registers under the new name.
    function setScreenName(name) {
        if (!browser) return;
        const trimmed = (name || '').trim();
        try {
            if (trimmed) localStorage.setItem('eck_screen_name', trimmed);
            else localStorage.removeItem('eck_screen_name');
        } catch {}
        reconnect();
    }

    function send(msg) {
        if (socket && socket.readyState === WebSocket.OPEN) {
            socket.send(JSON.stringify(msg));
        } else {
            console.warn('[WS] Cannot send, not connected');
        }
    }

    return {
        subscribe,
        connect,
        close,
        reconnect,
        send,
        getScreenId,
        getScreenName,
        setScreenName
    };
}

export const wsStore = createWsStore();
