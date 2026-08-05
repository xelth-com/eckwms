// Lightweight i18n core (no external deps).
//
// Dictionary layout (one file per language per namespace — workers/features add
// files, nobody edits this registry):
//   src/lib/i18n/locales/<lang>/<namespace>.js   e.g. locales/de/shell.js
// Each file default-exports a FLAT object of snake_case keys. The key exposed
// to components is "<namespace>.<key>", e.g. $t('shell.nav_repairs').
//
// Usage in .svelte files:
//   import { t } from '$lib/i18n';
//   <button>{$t('shell.save')}</button>
//   {$t('users.delete_confirm', { name: user.name })}   // {name} interpolation
// In plain .js modules (stores, handlers outside components):
//   import { tr } from '$lib/i18n';  tr('shell.save')
//
// Locale resolution order: explicit user choice (persisted per user id) →
// user.preferredLanguage → 'en'. Missing key falls back lang → en → raw key,
// so untranslated pages degrade to English, never break.

import { writable, derived, get } from 'svelte/store';
import { browser } from '$app/environment';

const FALLBACK = 'en';
const OVERRIDE_KEY = 'eck_locale_override'; // JSON {userId, locale}

// Eagerly bundle every locales/<lang>/<ns>.js into { lang: { "ns.key": str } }.
const registry = {};
const modules = import.meta.glob('./locales/*/*.js', { eager: true });
for (const [path, mod] of Object.entries(modules)) {
  const m = path.match(/\.\/locales\/([^/]+)\/([^/]+)\.js$/);
  if (!m) continue;
  const [, lang, ns] = m;
  registry[lang] ??= {};
  for (const [key, val] of Object.entries(mod.default ?? {})) {
    registry[lang][`${ns}.${key}`] = val;
  }
}

// Languages the UI can render = BUNDLED dictionaries (known at build time) ∪
// DB label languages (customer-added at runtime, machine-translated). The DB set
// is discovered after boot via refreshAvailableLocales() and merged in, so the
// picker grows without a rebuild.
const bundledLocales = Object.keys(registry).sort();
const knownLocales = new Set(bundledLocales);

/** Available locale codes (sorted), reactive. Grows as customer-added languages
 *  are discovered. Components subscribe via `$availableLocales`. */
export const availableLocales = writable([...knownLocales].sort());

/** Merge discovered codes into the known set; bump the store if anything's new. */
function addKnownLocales(codes) {
  let changed = false;
  for (const raw of codes ?? []) {
    const c = (raw || '').toLowerCase().slice(0, 2);
    if (c.length === 2 && !knownLocales.has(c)) {
      knownLocales.add(c);
      changed = true;
    }
  }
  if (changed) availableLocales.set([...knownLocales].sort());
}

/** Fetch the languages that actually exist in the DB label store and merge them
 *  into the available set. Safe to call repeatedly (boot, after adding a
 *  language). `token` is optional (the endpoint sits behind auth). Returns the
 *  parsed response so callers can reuse `languages` (kiosk cycle order). */
export async function refreshAvailableLocales(token = null) {
  if (!browser) return null;
  try {
    const res = await fetch('/api/i18n/languages', {
      headers: token ? { Authorization: `Bearer ${token}` } : {},
    });
    if (!res.ok) return null;
    const data = await res.json();
    if (data?.success && Array.isArray(data.dict_languages)) {
      addKnownLocales(data.dict_languages);
    }
    return data ?? null;
  } catch {
    return null; // offline / unauth — bundled set keeps working
  }
}

/** Native display names for common languages (generic — not firm-specific). */
const NATIVE_NAMES = {
  en: 'English', de: 'Deutsch', ko: '한국어', ru: 'Русский',
  fr: 'Français', es: 'Español', it: 'Italiano', pl: 'Polski',
  tr: 'Türkçe', uk: 'Українська', nl: 'Nederlands', pt: 'Português',
  zh: '中文', ja: '日本語', ar: 'العربية', ro: 'Română', hr: 'Hrvatski',
  bg: 'Български', hi: 'हिन्दी', cs: 'Čeština', el: 'Ελληνικά',
  he: 'עברית', hu: 'Magyar', sv: 'Svenska', da: 'Dansk', fi: 'Suomi',
  sk: 'Slovenčina', lt: 'Lietuvių', lv: 'Latviešu', et: 'Eesti',
  sl: 'Slovenščina', sr: 'Srpski', bs: 'Bosanski', no: 'Norsk',
};
export function localeName(code) {
  return NATIVE_NAMES[code] ?? code.toUpperCase();
}

// Display default when nothing else decides (German-market product). FALLBACK
// stays 'en' — it is the missing-KEY fallback, not the display default.
const DEFAULT_LOCALE = 'de';

/**
 * Initial locale, pda.repair-style ladder: explicit device choice (the
 * override survives before auth resolves) → browser's first language tag →
 * German. applyUserLocale() refines this after login.
 */
function initialLocale() {
  if (!browser) return FALLBACK;
  try {
    const saved = JSON.parse(localStorage.getItem(OVERRIDE_KEY) ?? 'null');
    const c = normalize(saved?.locale);
    if (c) return c;
  } catch { /* fall through */ }
  return normalize(navigator.language) ?? DEFAULT_LOCALE;
}

export const locale = writable(initialLocale());

// Mirror the active locale onto <html lang> so CSS :lang() rules can pick
// language-correct CJK font variants (see --font-family in app.css) and
// screen readers announce the right language.
if (browser) {
  locale.subscribe((l) => {
    document.documentElement.lang = l || FALLBACK;
  });
}

// Runtime source of truth is SurrealDB (i18n_label via GET /api/i18n/dict/:lang).
// The bundled registry above is the SEED/fallback so the first paint and offline
// use are never blank. DB values overlay bundled ones per key.
const dbOverlay = writable({}); // { lang: { "ns.key": value } }
const DICT_CACHE_PREFIX = 'eck_dict_';
const loadedLangs = new Set();

async function loadDict(lang) {
  if (!browser || !lang || loadedLangs.has(lang)) return;
  loadedLangs.add(lang);
  try {
    const cached = localStorage.getItem(DICT_CACHE_PREFIX + lang);
    if (cached) {
      const { labels } = JSON.parse(cached);
      if (labels) dbOverlay.update((o) => ({ ...o, [lang]: labels }));
    }
  } catch { /* bad cache — ignore */ }
  try {
    const res = await fetch(`/api/i18n/dict/${lang}`);
    if (!res.ok) return;
    const data = await res.json();
    if (data?.success && data.labels && Object.keys(data.labels).length) {
      dbOverlay.update((o) => ({ ...o, [lang]: data.labels }));
      try {
        localStorage.setItem(
          DICT_CACHE_PREFIX + lang,
          JSON.stringify({ ts: Date.now(), labels: data.labels }),
        );
      } catch { /* storage full — cache skip */ }
    }
  } catch { /* endpoint unavailable — bundled seed keeps working */ }
}

if (browser) {
  loadDict(FALLBACK);
  locale.subscribe((l) => loadDict(l));
}

function interpolate(str, vars) {
  if (!vars) return str;
  return str.replace(/\{(\w+)\}/g, (_, k) => (vars[k] ?? `{${k}}`));
}

/** Reactive translator: $t('ns.key', {vars}). DB overlay wins over bundled seed. */
export const t = derived([locale, dbOverlay], ([l, db]) => (key, vars) => {
  const str =
    db[l]?.[key] ?? registry[l]?.[key] ??
    db[FALLBACK]?.[key] ?? registry[FALLBACK]?.[key] ?? key;
  return interpolate(str, vars);
});

/** Non-reactive translate for plain .js modules / event handlers. */
export function tr(key, vars) {
  return get(t)(key, vars);
}

function normalize(code) {
  const c = (code || '').toLowerCase().slice(0, 2);
  return knownLocales.has(c) ? c : null;
}

/** Explicit switch (settings dropdown / kiosk cycle button). Persists per user. */
export function setLocale(code, userId = null) {
  const c = normalize(code);
  if (!c) return;
  const changed = c !== get(locale);
  locale.set(c);
  if (browser) {
    try {
      localStorage.setItem(OVERRIDE_KEY, JSON.stringify({ userId, locale: c }));
    } catch { /* storage unavailable — session-only switch */ }
    // Imperatively-built UI (Leaflet popups/labels baked via tr(), filter
    // chips) doesn't track the locale store — reload so EVERYTHING re-renders
    // in the new language. Only on explicit switches; applyUserLocale (login
    // path) sets the store directly and never reloads.
    if (changed) location.reload();
  }
}

/**
 * Called on login / auth init with the authenticated user object.
 * Applies that user's saved override if it is THEIRS, else their
 * preferredLanguage, else keeps the browser-derived initial locale.
 * Shared observer accounts (public demo "Gast", kiosk walk-ups) skip the
 * preference: one stored language must not override every visitor's browser.
 */
export function applyUserLocale(user) {
  if (!browser || !user) return;
  try {
    const raw = localStorage.getItem(OVERRIDE_KEY);
    if (raw) {
      const saved = JSON.parse(raw);
      if (saved?.userId === (user.id ?? null) && normalize(saved.locale)) {
        locale.set(normalize(saved.locale));
        return;
      }
    }
  } catch { /* fall through to preference */ }
  if (user.role === 'observer') return;
  const pref = normalize(user.preferredLanguage);
  if (pref) locale.set(pref);
}

/** Advance to the next language in `cycle` (kiosk observer button). */
export function cycleLocale(cycle, userId = null) {
  const list = (cycle?.length ? cycle : get(availableLocales))
    .map(normalize)
    .filter(Boolean);
  if (!list.length) return;
  const cur = get(locale);
  const next = list[(list.indexOf(cur) + 1) % list.length];
  setLocale(next, userId);
}
