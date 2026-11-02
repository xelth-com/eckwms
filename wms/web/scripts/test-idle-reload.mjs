// Quick pure-function smoke test for the idle-reload / resume-countdown
// helpers (no DOM, no SvelteKit runtime). Run with:
//   node wms/web/scripts/test-idle-reload.mjs
import assert from 'node:assert';
import {
    shouldReloadNow,
    shouldResumeImmediately,
    isEditableElement,
    IDLE_RELOAD_MS,
    RESUME_MAX_AGE_MS,
} from '../src/lib/idleReload.js';

// -- shouldReloadNow -------------------------------------------------------

// Not idle long enough yet.
assert.strictEqual(
    shouldReloadNow({ lastActivityAt: 1000, now: 1000 + IDLE_RELOAD_MS - 1, activeElement: null }),
    false
);

// Idle long enough, nothing focused -> reload.
assert.strictEqual(
    shouldReloadNow({ lastActivityAt: 1000, now: 1000 + IDLE_RELOAD_MS, activeElement: null }),
    true
);

// Idle long enough, but user is typing in an input -> never reload.
assert.strictEqual(
    shouldReloadNow({
        lastActivityAt: 1000,
        now: 1000 + IDLE_RELOAD_MS + 5000,
        activeElement: { tagName: 'INPUT' },
    }),
    false
);
assert.strictEqual(isEditableElement({ tagName: 'TEXTAREA' }), true);
assert.strictEqual(isEditableElement({ tagName: 'SELECT' }), true);
assert.strictEqual(isEditableElement({ tagName: 'DIV', isContentEditable: true }), true);
assert.strictEqual(isEditableElement({ tagName: 'BODY' }), false);
assert.strictEqual(isEditableElement(null), false);

// -- shouldResumeImmediately -----------------------------------------------

// No stored value -> normal countdown.
assert.strictEqual(shouldResumeImmediately(null), false);
assert.strictEqual(shouldResumeImmediately(''), false);

// Fresh marker (just stamped) -> resume now.
const now = 10_000_000;
assert.strictEqual(shouldResumeImmediately(String(now - 1000), now), true);

// Marker right at the edge of the window -> still resumes.
assert.strictEqual(shouldResumeImmediately(String(now - (RESUME_MAX_AGE_MS - 1)), now), true);

// Marker older than 3 minutes -> falls back to the countdown.
assert.strictEqual(shouldResumeImmediately(String(now - (RESUME_MAX_AGE_MS + 1)), now), false);

// Garbage value -> never crashes, just no resume.
assert.strictEqual(shouldResumeImmediately('not-a-number', now), false);

console.log('idle-reload helper tests: OK');
