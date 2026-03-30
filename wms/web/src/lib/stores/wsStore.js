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

    function connect() {
        if (!browser) return;
        if (socket && (socket.readyState === WebSocket.CONNECTING || socket.readyState === WebSocket.OPEN)) return;

        // Determine protocol (ws or wss) and construct URL
        const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
        const host = window.location.host;
        // Use /E/ws path (HTTP_PATH_PREFIX=/E from backend config)
        // CaseInsensitiveMiddleware on backend handles both /E/ws and /e/ws
        const url = `${protocol}//${host}/E/ws`;

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
        send
    };
}

export const wsStore = createWsStore();
