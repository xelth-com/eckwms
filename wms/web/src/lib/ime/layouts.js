// Physical-key layouts for the in-app IME (see ime.js).
//
// The kiosk runs cage + Chromium with ONE fixed XKB layout and no OS IME, so
// the app maps by physical key POSITION (KeyboardEvent.code) — independent of
// whatever layout the OS thinks is active. Each layout maps a `code` to a
// [normal, shifted] pair. Letters (entries whose char has a lower/upper case)
// additionally respect CapsLock: uppercase when Shift XOR CapsLock. Symbols and
// jamo (no case) just switch on Shift.
//
// Adding a language later (e.g. `uk`) is a DATA change: add a LAYOUTS entry —
// imeLangFor() then picks it up automatically.

// ── Russian — standard Windows ЙЦУКЕН, by physical position ──────────────────
const ru = {
  Backquote: ['ё', 'Ё'],
  // Digit row: shifted row is ! " № ; % : ? * ( )
  Digit1: ['1', '!'],
  Digit2: ['2', '"'],
  Digit3: ['3', '№'],
  Digit4: ['4', ';'],
  Digit5: ['5', '%'],
  Digit6: ['6', ':'],
  Digit7: ['7', '?'],
  Digit8: ['8', '*'],
  Digit9: ['9', '('],
  Digit0: ['0', ')'],
  Minus: ['-', '_'],
  Equal: ['=', '+'],
  // Top letter row
  KeyQ: ['й', 'Й'],
  KeyW: ['ц', 'Ц'],
  KeyE: ['у', 'У'],
  KeyR: ['к', 'К'],
  KeyT: ['е', 'Е'],
  KeyY: ['н', 'Н'],
  KeyU: ['г', 'Г'],
  KeyI: ['ш', 'Ш'],
  KeyO: ['щ', 'Щ'],
  KeyP: ['з', 'З'],
  BracketLeft: ['х', 'Х'],
  BracketRight: ['ъ', 'Ъ'],
  Backslash: ['\\', '/'],
  // Home letter row
  KeyA: ['ф', 'Ф'],
  KeyS: ['ы', 'Ы'],
  KeyD: ['в', 'В'],
  KeyF: ['а', 'А'],
  KeyG: ['п', 'П'],
  KeyH: ['р', 'Р'],
  KeyJ: ['о', 'О'],
  KeyK: ['л', 'Л'],
  KeyL: ['д', 'Д'],
  Semicolon: ['ж', 'Ж'],
  Quote: ['э', 'Э'],
  // Bottom letter row
  KeyZ: ['я', 'Я'],
  KeyX: ['ч', 'Ч'],
  KeyC: ['с', 'С'],
  KeyV: ['м', 'М'],
  KeyB: ['и', 'И'],
  KeyN: ['т', 'Т'],
  KeyM: ['ь', 'Ь'],
  Comma: ['б', 'Б'],
  Period: ['ю', 'Ю'],
  Slash: ['.', ','],
};

// ── Korean — 2-beolsik (두벌식), by physical position ─────────────────────────
// Emits COMPATIBILITY jamo (U+3130 block); hangul.js assembles syllables.
// Digits and punctuation are intentionally NOT mapped — they pass through to
// the OS so numbers/symbols type normally in Korean mode.
const ko = {
  KeyQ: ['ㅂ', 'ㅃ'],
  KeyW: ['ㅈ', 'ㅉ'],
  KeyE: ['ㄷ', 'ㄸ'],
  KeyR: ['ㄱ', 'ㄲ'],
  KeyT: ['ㅅ', 'ㅆ'],
  KeyY: ['ㅛ', 'ㅛ'],
  KeyU: ['ㅕ', 'ㅕ'],
  KeyI: ['ㅑ', 'ㅑ'],
  KeyO: ['ㅐ', 'ㅒ'],
  KeyP: ['ㅔ', 'ㅖ'],
  KeyA: ['ㅁ', 'ㅁ'],
  KeyS: ['ㄴ', 'ㄴ'],
  KeyD: ['ㅇ', 'ㅇ'],
  KeyF: ['ㄹ', 'ㄹ'],
  KeyG: ['ㅎ', 'ㅎ'],
  KeyH: ['ㅗ', 'ㅗ'],
  KeyJ: ['ㅓ', 'ㅓ'],
  KeyK: ['ㅏ', 'ㅏ'],
  KeyL: ['ㅣ', 'ㅣ'],
  KeyZ: ['ㅋ', 'ㅋ'],
  KeyX: ['ㅌ', 'ㅌ'],
  KeyC: ['ㅊ', 'ㅊ'],
  KeyV: ['ㅍ', 'ㅍ'],
  KeyB: ['ㅠ', 'ㅠ'],
  KeyN: ['ㅜ', 'ㅜ'],
  KeyM: ['ㅡ', 'ㅡ'],
};

/** Registry of available layouts, keyed by IME language. */
export const LAYOUTS = { ru, ko };

/**
 * Map a UI locale code to an IME language with a layout, or null if none.
 * Registry-driven: a locale is IME-capable iff LAYOUTS has an entry for it.
 */
export function imeLangFor(locale) {
  return locale && LAYOUTS[locale] ? locale : null;
}

/**
 * Pure key resolver — no DOM. Returns the character a physical key should emit
 * in the given layout, or null if the key is not mapped (pass it through).
 * Letters honour CapsLock (upper when Shift XOR CapsLock); symbols/jamo switch
 * on Shift only.
 */
export function resolveKey(lang, code, mods = {}) {
  const layout = LAYOUTS[lang];
  if (!layout) return null;
  const entry = layout[code];
  if (!entry) return null;
  const [normal, shifted] = entry;
  const shift = !!mods.shift;
  const caps = !!mods.caps;
  const hasCase = normal.toLowerCase() !== normal.toUpperCase();
  const useShifted = hasCase ? shift !== caps : shift;
  return useShifted ? shifted : normal;
}

/**
 * True when a character belongs to a NON-Latin script — i.e. the OS layout is
 * already producing Cyrillic/Hangul/etc. and the app must not remap by position.
 * The cutoff is U+024F (end of Latin Extended-B): everything ≤ that (ASCII,
 * Latin-1 including § ° ß, and the ö/ä/ü of European layouts) is treated as "a
 * Latin layout" and IS remapped by physical key position; anything above it is
 * a genuinely non-Latin character and passes through untouched.
 */
export function isNonLatin(ch) {
  if (!ch) return false;
  return ch.codePointAt(0) > 0x024f;
}
