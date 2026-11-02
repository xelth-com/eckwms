// In-app IME for physical keyboards.
//
// The production kiosk (cage + Chromium) has ONE fixed XKB layout and no OS IME,
// so when the UI language is Russian or Korean this module intercepts keydown by
// physical POSITION (KeyboardEvent.code) and inserts the right character at the
// caret, with English always one toggle away. It is designed to stay harmless on
// office PCs where users have their own OS layouts: it only acts when turned on,
// and passes through the moment the OS is already producing non-ASCII itself.
//
// State is exposed as Svelte stores; initIme() wires the single global keydown
// listener (capture phase) plus the listeners that end an in-flight composition.

import { writable, derived, get } from 'svelte/store';
import { browser } from '$app/environment';
import { locale } from '$lib/i18n';
import { imeLangFor, resolveKey, isNonLatin } from './layouts';
import { createComposer } from './hangul';

// ── Stores ───────────────────────────────────────────────────────────────────
/** Active IME language for the current UI locale ('ru' | 'ko' | null). */
export const imeLang = derived(locale, ($l) => imeLangFor($l));
/** Whether an alternate layout even exists for the current locale. */
export const imeAvailable = derived(imeLang, ($l) => $l !== null);
/** Alternate layout active (true) vs English pass-through (false). */
export const imeOn = writable(false);

// ── localStorage keys ─────────────────────────────────────────────────────────
const KIOSK_FLAG = 'eck_kiosk_screen'; // set by authStore when kiosk-token succeeds
const CHOICE_PREFIX = 'eck_ime_on_'; // per-language explicit user choice ('1'|'0')

function isKioskScreen() {
  try {
    return localStorage.getItem(KIOSK_FLAG) === '1';
  } catch {
    return false;
  }
}
function readChoice(lang) {
  try {
    return localStorage.getItem(CHOICE_PREFIX + lang);
  } catch {
    return null;
  }
}
function writeChoice(lang, on) {
  try {
    localStorage.setItem(CHOICE_PREFIX + lang, on ? '1' : '0');
  } catch {
    /* storage unavailable — session-only */
  }
}

// ── Composition state (Korean) ────────────────────────────────────────────────
const composer = createComposer();
let composerActive = false; // is a syllable currently open?
let composerText = ''; // the single live char currently in the field (0 or 1 chars)
let currentLang = null; // mirror of imeLang for synchronous access
// Set while WE are mutating the field/caret, so our own programmatic
// selectionchange (from selecting the composing char before replacing it) does
// not look like a user caret move and abort the composition.
let internalEdit = false;
function beginInternalEdit() {
  internalEdit = true;
  // selectionchange is dispatched asynchronously; clear the guard on the next
  // macrotask, by which point the field is in its final, consistent state.
  setTimeout(() => {
    internalEdit = false;
  }, 0);
}

/** End any in-flight composition; the field text stays as-is (becomes final). */
export function endComposition() {
  composer.reset();
  composerActive = false;
  composerText = '';
}

// ── imeOn default + persistence ────────────────────────────────────────────────
// Default when a layout language becomes active: ON on a kiosk screen, OFF on an
// office PC — unless the user made an explicit choice for this language before.
function applyDefaultFor(lang) {
  if (!lang) {
    imeOn.set(false);
    return;
  }
  const stored = readChoice(lang);
  if (stored === '1') imeOn.set(true);
  else if (stored === '0') imeOn.set(false);
  else imeOn.set(isKioskScreen());
}

/** Set the IME on/off, persist the choice for the current language, end comp. */
export function setImeOn(on) {
  imeOn.set(on);
  if (currentLang) writeChoice(currentLang, on);
  endComposition();
}

/** Toggle the IME (no-op when no layout is available for this locale). */
export function toggleIme() {
  if (currentLang === null) return;
  setImeOn(!get(imeOn));
}

// ── Eligibility ────────────────────────────────────────────────────────────────
const BAD_TYPES = new Set(['password', 'email', 'url', 'number', 'tel',
  'date', 'datetime-local', 'month', 'week', 'time', 'color', 'range', 'file', 'checkbox', 'radio']);
const BAD_INPUTMODES = new Set(['numeric', 'decimal', 'tel', 'email', 'url']);
const BAD_AUTOCOMPLETE = /username|current-password|new-password|one-time-code/;

function isEligible(el) {
  if (!el || (el.tagName !== 'INPUT' && el.tagName !== 'TEXTAREA')) return false;
  if (el.disabled || el.readOnly) return false;
  if (el.tagName === 'INPUT') {
    const type = (el.getAttribute('type') || 'text').toLowerCase();
    if (BAD_TYPES.has(type)) return false; // only text/search/(missing) allowed
    if (!['text', 'search', ''].includes(type)) return false;
  }
  if (typeof el.closest === 'function' && el.closest('[data-ime="off"]')) return false;
  const ac = (el.getAttribute('autocomplete') || '').toLowerCase();
  if (BAD_AUTOCOMPLETE.test(ac)) return false;
  const im = (el.getAttribute('inputmode') || '').toLowerCase();
  if (BAD_INPUTMODES.has(im)) return false;
  return true;
}

// Any element that captures typed text (so a plain Space would insert there).
function isEditable(el) {
  return !!el && (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA' || el.isContentEditable === true);
}

// ── Text insertion ──────────────────────────────────────────────────────────
// Prefer execCommand('insertText'): it keeps native undo, fires beforeinput/
// input, and honours maxlength — so Svelte bind:value and component keydown
// handlers see the change. Fall back to setRangeText + a dispatched InputEvent.
function insertText(el, text) {
  if (text === '') return deleteSelection(el);
  let ok = false;
  try {
    ok = document.execCommand('insertText', false, text);
  } catch {
    ok = false;
  }
  if (ok) return;
  const start = el.selectionStart ?? el.value.length;
  const end = el.selectionEnd ?? el.value.length;
  try {
    el.setRangeText(text, start, end, 'end');
  } catch {
    el.value = el.value.slice(0, start) + text + el.value.slice(end);
    const pos = start + text.length;
    try {
      el.setSelectionRange(pos, pos);
    } catch {
      /* some inputs don't support selection */
    }
  }
  el.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertText', data: text }));
}

function deleteSelection(el) {
  let ok = false;
  try {
    ok = document.execCommand('delete', false);
  } catch {
    ok = false;
  }
  if (ok) return;
  const start = el.selectionStart ?? 0;
  const end = el.selectionEnd ?? 0;
  try {
    el.setRangeText('', start, end, 'end');
  } catch {
    el.value = el.value.slice(0, start) + el.value.slice(end);
  }
  el.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'deleteContentBackward', data: null }));
}

function selectBeforeCaret(el, n) {
  const caret = el.selectionStart;
  try {
    el.setSelectionRange(caret - n, caret);
  } catch {
    /* ignore */
  }
}

// The composing char lives in the field as a real character; verify the field
// still matches what the composer believes before we replace it.
function stateConsistent(el) {
  if (document.activeElement !== el) return false;
  if (composerText === '') return true;
  const caret = el.selectionStart;
  if (caret == null || caret !== el.selectionEnd) return false;
  if (caret < composerText.length) return false;
  return el.value.slice(caret - composerText.length, caret) === composerText;
}

// ── Korean insertion (composing in place) ────────────────────────────────────
function insertKorean(el, jamo) {
  if (composerActive && !stateConsistent(el)) {
    // Field drifted from the composer (mouse move, external edit) → fresh start.
    composer.reset();
    composerActive = false;
    composerText = '';
  }
  const prev = composerText;
  const res = composer.input(jamo);
  beginInternalEdit();
  if (prev.length > 0) selectBeforeCaret(el, prev.length);
  insertText(el, res.commit + res.composing);
  composerText = res.composing;
  composerActive = res.composing.length > 0;
}

function handleKoreanBackspace(el) {
  if (!composerActive || composerText === '' || !stateConsistent(el)) return false;
  const res = composer.backspace();
  if (!res.handled) return false;
  beginInternalEdit();
  selectBeforeCaret(el, composerText.length);
  insertText(el, res.composing); // '' → deletes the composing char
  composerText = res.composing;
  composerActive = res.composing.length > 0;
  return true;
}

// ── Modifier-tap detection (Alt+Shift / Ctrl+Shift / AltRight) ────────────────
const MOD_CODES = new Set(['ShiftLeft', 'ShiftRight', 'ControlLeft', 'ControlRight',
  'AltLeft', 'AltRight', 'MetaLeft', 'MetaRight']);
let comboPeak = new Set(); // modifier codes held together this press-cycle
let sawOtherKey = false; // a non-modifier key (a real char/chord) happened
let firedThisCycle = false; // a toggle already fired for this press-cycle
let pressed = new Set(); // currently-down keys, to know when the cycle ends

function trackDown(code) {
  if (!MOD_CODES.has(code)) {
    sawOtherKey = true;
  } else {
    comboPeak.add(code);
  }
  pressed.add(code);
}

function trackUpAndMaybeToggle(code) {
  // A "tap" is modifiers pressed & released with no other key in between.
  if (!sawOtherKey && !firedThisCycle && get(imeAvailable)) {
    const hasShift = comboPeak.has('ShiftLeft') || comboPeak.has('ShiftRight');
    const hasCtrl = comboPeak.has('ControlLeft') || comboPeak.has('ControlRight');
    const hasAlt = comboPeak.has('AltLeft') || comboPeak.has('AltRight');
    const hasMeta = comboPeak.has('MetaLeft') || comboPeak.has('MetaRight');
    const altShift = hasAlt && hasShift && !hasCtrl && !hasMeta;
    const ctrlShift = hasCtrl && hasShift && !hasAlt && !hasMeta;
    // AltRight alone == the 한/영 key on Korean boards. On German boards AltRight
    // is AltGr and the OS reports it as Ctrl+Alt, so comboPeak would contain a
    // Control code too — the exact-{AltRight} test excludes that case.
    const altRightSolo =
      currentLang === 'ko' && comboPeak.size === 1 && comboPeak.has('AltRight');
    if (altShift || ctrlShift || altRightSolo) {
      firedThisCycle = true;
      endComposition();
      toggleIme();
    }
  }
  pressed.delete(code);
  if (pressed.size === 0) {
    comboPeak = new Set();
    sawOtherKey = false;
    firedThisCycle = false;
  }
}

// ── The main keydown handler (capture phase) ─────────────────────────────────
function onKeydown(e) {
  if (!get(imeAvailable)) return;

  if (!e.repeat) trackDown(e.code);

  // Toggle hotkeys work regardless of on/off state; each ends any composition
  // and preventDefaults so no stray character appears.
  // Shift+Space: BUT if focus is in an editable field the IME won't act on
  // (password, email, data-ime="off", …), let it type a normal space instead —
  // toggling there would silently corrupt e.g. a password typed with Shift held.
  if (e.code === 'Space' && e.shiftKey && !e.ctrlKey && !e.altKey && !e.metaKey &&
      !e.getModifierState('AltGraph')) {
    if (isEditable(e.target) && !isEligible(e.target)) return; // normal space
    e.preventDefault();
    endComposition();
    toggleIme();
    return;
  }
  if (e.code === 'Lang1' || e.key === 'HangulMode' || e.key === 'Hangul') {
    e.preventDefault();
    endComposition();
    toggleIme();
    return;
  }
  // (Alt+Shift / Ctrl+Shift / AltRight taps fire on keyup, in onKeyup.)

  if (!get(imeOn)) return;
  const lang = currentLang;
  if (!lang) return;
  const el = e.target;
  if (!isEligible(el)) return;

  // Pass-through: an OS IME/dead-key is already composing. Note: on layouts
  // where Backquote is a dead key (e.g. German ^), it arrives as 'Dead' and is
  // passed through — so the ё position (Backquote) is unavailable on those.
  if (e.isComposing || e.key === 'Process' || e.key === 'Dead') {
    endComposition();
    return;
  }
  // Pass-through: shortcut chords (Ctrl/Cmd/Alt) and AltGr symbols.
  if (e.ctrlKey || e.metaKey || e.altKey || e.getModifierState('AltGraph')) {
    endComposition();
    return;
  }

  // Backspace: for Korean, decompose the live syllable in place; otherwise let
  // the browser delete normally.
  if (e.key === 'Backspace') {
    if (lang === 'ko' && composerActive) {
      if (handleKoreanBackspace(el)) {
        e.preventDefault();
        return;
      }
    }
    endComposition();
    return;
  }

  // Navigation / whitespace keys end the composition and pass through.
  if (e.key.length !== 1) {
    // Enter, Tab, Escape, arrows, Home, End, F-keys, etc. all have multi-char
    // key names. They finalize the syllable and are handled by the app/browser.
    endComposition();
    return;
  }

  // Crucial for office PCs: only let a key through when the OS layout is ALREADY
  // producing a genuinely NON-LATIN character (Cyrillic, Hangul, …). Latin-1 /
  // Latin-Extended output (a German ö/ä/ü, ß, § …) still comes from a "Latin"
  // layout and must be remapped by physical position, so it is NOT passed here.
  if (isNonLatin(e.key)) {
    if (lang === 'ko') endComposition();
    return;
  }

  // Map by physical position.
  const caps = typeof e.getModifierState === 'function' && e.getModifierState('CapsLock');
  const ch = resolveKey(lang, e.code, { shift: e.shiftKey, caps });
  if (ch == null) {
    // Unmapped key (Space in ru, digits/punct in ko, …) → pass through, but the
    // syllable is done.
    endComposition();
    return;
  }

  e.preventDefault();
  if (lang === 'ko') insertKorean(el, ch);
  else insertText(el, ch);
}

function onKeyup(e) {
  if (!get(imeAvailable)) return;
  trackUpAndMaybeToggle(e.code);
}

// A caret move we did NOT cause (click, arrow, external edit) ends composition.
function onSelectionChange() {
  if (internalEdit || !composerActive) return;
  const el = document.activeElement;
  if (!isEligible(el) || !stateConsistent(el)) endComposition();
}

function onPointerOrFocus() {
  endComposition();
}

function onKioskScreen() {
  endComposition();
  applyDefaultFor(currentLang);
}

// ── Wiring ─────────────────────────────────────────────────────────────────
let started = false;

/**
 * Wire the IME once (call from the root layout's onMount — it must run on the
 * login page too). Returns a cleanup function.
 */
export function initIme() {
  if (!browser || started) return () => {};
  started = true;

  const unsubLang = imeLang.subscribe((lang) => {
    currentLang = lang;
    endComposition();
    applyDefaultFor(lang);
  });

  // authStore learns asynchronously that this browser is a kiosk screen; when
  // that lands after our first default was computed, re-apply it (an explicit
  // user choice for the language still wins inside applyDefaultFor).
  window.addEventListener('eck:kiosk-screen', onKioskScreen);
  window.addEventListener('keydown', onKeydown, true); // capture phase
  window.addEventListener('keyup', onKeyup, true);
  window.addEventListener('focusin', onPointerOrFocus, true);
  window.addEventListener('focusout', onPointerOrFocus, true);
  window.addEventListener('mousedown', onPointerOrFocus, true);
  window.addEventListener('pointerdown', onPointerOrFocus, true);
  document.addEventListener('selectionchange', onSelectionChange);

  return () => {
    unsubLang();
    window.removeEventListener('eck:kiosk-screen', onKioskScreen);
    window.removeEventListener('keydown', onKeydown, true);
    window.removeEventListener('keyup', onKeyup, true);
    window.removeEventListener('focusin', onPointerOrFocus, true);
    window.removeEventListener('focusout', onPointerOrFocus, true);
    window.removeEventListener('mousedown', onPointerOrFocus, true);
    window.removeEventListener('pointerdown', onPointerOrFocus, true);
    document.removeEventListener('selectionchange', onSelectionChange);
    started = false;
  };
}
