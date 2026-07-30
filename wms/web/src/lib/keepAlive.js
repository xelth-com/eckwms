// ── Dashboard keep-alive registry ───────────────────────────────────────────
//
// Tabs listed here are mounted ONCE and kept alive across navigation (hidden via
// display:none, not unmounted), so leaving and returning is instant and keeps
// their state (scroll, filters, the map's view, fetched data). An LRU keeps at
// most KEEP_ALIVE_MAX mounted; idle ones are dropped after KEEP_ALIVE_TTL_MS.
//
// ── To cache another tab (3 steps) ──────────────────────────────────────────
//   1. Move that route's `+page.svelte` body into `lib/components/XxxView.svelte`
//      and add `export let active = true;` near the top (the cache passes it;
//      use it to pause background timers/WS while hidden — see DashboardMap).
//   2. Replace the route's `+page.svelte` with an empty stub (a comment).
//   3. Add ONE line to KEEP_ALIVE_VIEWS below: { path, component }.
//   That's it — the layout renders/caches it automatically.
//
// `path` is the route path WITHOUT the base prefix (e.g. '/dashboard/support').
// `fullbleed: true` = the view fills the pane edge-to-edge (the map); omit otherwise.

import DashboardMap from './components/DashboardMap.svelte';
import SupportListView from './components/SupportListView.svelte';
import RepairsListView from './components/RepairsListView.svelte';

export const KEEP_ALIVE_VIEWS = [
    { path: '/dashboard',         component: DashboardMap,    fullbleed: true },
    { path: '/dashboard/support', component: SupportListView },
    { path: '/dashboard/repairs', component: RepairsListView },
];

export const KEEP_ALIVE_MAX = 3;                 // max tabs kept mounted (LRU)
export const KEEP_ALIVE_TTL_MS = 10 * 60 * 1000; // drop a hidden tab after 10 min idle

// Match a route path (base already stripped) to its registry entry, or null.
export function keepAliveEntry(relPath) {
    return KEEP_ALIVE_VIEWS.find((v) => v.path === relPath) || null;
}
