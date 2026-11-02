// Pure idle-detection helpers for the "reload on new deploy" flow (root
// layout). Kept side-effect free and DOM-independent so they can be unit
// tested with plain objects — see wms/web/scripts/test-idle-reload.mjs.

/** How long (ms) with no pointer/keyboard/touch activity before an
 * auto-reload is allowed to fire. */
export const IDLE_RELOAD_MS = 45000;

/** sessionStorage key the login page checks to skip the kiosk countdown. */
export const RESUME_STORAGE_KEY = 'eck_resume_after_reload';

/** How long (ms) the resume marker stays valid on the login page. */
export const RESUME_MAX_AGE_MS = 3 * 60 * 1000;

/**
 * True when `el` is something the user could be actively typing into —
 * reloading out from under that would lose input.
 */
export function isEditableElement(el) {
    if (!el) return false;
    const tag = typeof el.tagName === 'string' ? el.tagName.toLowerCase() : '';
    if (tag === 'input' || tag === 'textarea' || tag === 'select') return true;
    if (el.isContentEditable) return true;
    return false;
}

/**
 * Decide whether it's safe to reload right now, given a new build is
 * waiting. Never true while the user is mid-interaction: either recently
 * active (pointer/keyboard/touch) or focused on an editable element.
 *
 * @param {{ lastActivityAt: number, now: number, activeElement?: any, idleMs?: number }} args
 */
export function shouldReloadNow({ lastActivityAt, now, activeElement, idleMs = IDLE_RELOAD_MS }) {
    if (now - lastActivityAt < idleMs) return false;
    if (isEditableElement(activeElement)) return false;
    return true;
}

/**
 * The login page's half of the resume handshake: given the raw
 * sessionStorage value (string | null) and the current time, decide
 * whether the kiosk auto-login should fire immediately instead of
 * counting down.
 */
export function shouldResumeImmediately(rawStoredValue, now = Date.now()) {
    if (!rawStoredValue) return false;
    const stampedAt = Number(rawStoredValue);
    if (!Number.isFinite(stampedAt)) return false;
    const age = now - stampedAt;
    return age >= 0 && age < RESUME_MAX_AGE_MS;
}
