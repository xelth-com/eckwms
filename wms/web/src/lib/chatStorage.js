// Shared localStorage namespace for the dashboard Brain Chat
// (wms/web/src/routes/dashboard/ai/+page.svelte).
//
// Chat history/summary/session are per-user (origin-wide keys would let the
// next account on a shared kiosk browser restore and re-send the previous
// account's conversation). This module is the single place that knows the
// key names and the namespacing scheme, so the chat page (read/write/
// restore) and the auth store (explicit-logout cleanup) cannot drift apart.

export const CHAT_ENGINE_KEY = 'eck_brain_engine';
export const CHAT_HISTORY_KEY = 'eck_brain_chat_history';
export const CHAT_SUMMARY_KEY = 'eck_brain_chat_summary';
export const CHAT_SESSION_KEY = 'eck_brain_chat_session';

// The namespace a user's chat storage lives under: their username, falling
// back to their id. `null` for no authenticated user.
export function usernameOf(user) {
    return user?.username || user?.id || null;
}

export function chatStorageKey(base, username) {
    return `${base}::${username}`;
}

// Pre-namespacing keys (origin-wide, no user suffix) — dropped on load.
export function dropLegacyChatKeys() {
    try {
        localStorage.removeItem(CHAT_ENGINE_KEY);
        localStorage.removeItem(CHAT_HISTORY_KEY);
        localStorage.removeItem(CHAT_SUMMARY_KEY);
        localStorage.removeItem(CHAT_SESSION_KEY);
    } catch {
        /* localStorage unavailable — nothing to drop */
    }
}

// Explicit logout (or a "start fresh" clear): remove this user's
// conversation — history, summary, session — but keep their engine
// preference, which is a UI setting rather than conversation data and
// should survive a re-login.
export function clearChatConversationFor(username) {
    if (!username) return;
    try {
        localStorage.removeItem(chatStorageKey(CHAT_HISTORY_KEY, username));
        localStorage.removeItem(chatStorageKey(CHAT_SUMMARY_KEY, username));
        localStorage.removeItem(chatStorageKey(CHAT_SESSION_KEY, username));
    } catch {
        /* localStorage unavailable — nothing to clear */
    }
}
