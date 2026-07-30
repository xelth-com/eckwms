<script>
import { onMount, onDestroy } from 'svelte';
import { api } from '$lib/api';
import { base } from '$app/paths';
import { toastStore } from '$lib/stores/toastStore';
import { wsStore } from '$lib/stores/wsStore';
import { t, tr } from '$lib/i18n';
// Pure MapLibre GL JS (no Leaflet). Vector OpenFreeMap base + native
// data-layers for clustered task markers, shipment routes, trips and live cars.
import 'maplibre-gl/dist/maplibre-gl.css';

// Kept-alive in the layout: mounted once, persists across navigation. `active`
// = we're on the dashboard route (visible). When hidden the host is
// display:none and MapLibre can't measure a hidden canvas, so on becoming
// visible again we must map.resize().
export let active = true;
$: if (active && map) {
    requestAnimationFrame(() => { try { map.resize(); } catch (e) {} });
}

let loading = true;
let mapLoading = true;
let mapContainer;
let map;
let maplibregl = null;
// Cluster drill-down state: the last-clicked cluster's leaves + anchor, so the
// "list ⇄ full card" popups (and their Back buttons) can re-render either view.
let lastClusterLeaves = null;
let lastClusterLngLat = null;
let activeTasks = [];        // Combined repairs + tickets
let activeShipments = [];

// Assignee filter chips.
let assigneeChips = [];
let selectedAssignees = new Set();
const UNASSIGNED_KEY = '__unassigned__';
const REPAIRS_KEY = '__repairs__';

// Built GeoJSON feature lists (the source of truth handed to MapLibre sources).
let taskFeatures = [];       // all task point features (filtered into the source)
let shipmentFeatures = [];   // route line features
let tripFeatures = [];       // historical trip line features
let tripMarkFeatures = [];   // per-trip START / FINISH flag point features
let progressFeatures = [];   // in-progress trips' current car position (until a TRIP_LIVE takes over)
const liveFeatures = new Map(); // trip_uuid → live-car point feature
const liveTimers = new Map();   // trip_uuid → expiry timeout
const liveLineFeatures = new Map(); // trip_uuid → growing trace LineString (up to the latest checkpoint)
const liveLineFetching = new Set();  // trip_uuid → in-flight points fetch (debounce)
let tripsInterval = null;            // setInterval handle: refresh the trips overlay every 5 min
let showTrips = true;   // Fahrten always shown (toggle removed from the map controls)
// Map search (replaces the removed dark/Fahrten/Live controls): filter the task
// markers by ticket/order number or customer, typed OR dictated (🎤 Web Speech).
let searchQuery = '';
let listening = false;
// Keep a live car + its growing trace on the map between checkpoints. An off-LAN
// trip reports via the relay only every few minutes (the PDA's checkpoint
// interval), so this must exceed that or the marker/line would blink out between
// updates. On-LAN per-fix TRIP_LIVE just refreshes it more often.
const LIVE_STALE_MS = 12 * 60_000;

// Shipment SLA thresholds (days)
const SHIP_HIDE_DELIVERED_AFTER_DAYS = 3;
const SHIP_ALERT_UNDELIVERED_AFTER_DAYS = 7;

// Dashboard SLA scale — loaded from /api/admin/config/dashboard_sla on mount.
let agingScaleDays = 7;
let repairAgingScaleDays = 7;
// Trip lines are coloured per vehicle and fade to nothing over this many days;
// operator-configurable via dashboard_sla.trip_fade_days (general settings).
let tripFadeDays = 3;

// Deterministic per-plate colour so every trip of one vehicle reads as one strand.
function plateColor(plate) {
    // Normalise before hashing so one vehicle reads as ONE hue regardless of
    // plate formatting (case / spaces / dashes: "M-AB 123" === "mab123").
    const p = (plate || '').toUpperCase().replace(/[\s-]/g, '');
    if (!p) return '#9ca3af'; // plate-less trip → neutral grey
    let h = 0;
    for (let i = 0; i < p.length; i++) h = (h * 31 + p.charCodeAt(i)) >>> 0;
    return `hsl(${h % 360}, 70%, 55%)`;
}
// Age-based opacity: fresh ≈ 0.9, linearly to 0 at tripFadeDays (0 → aged out).
function tripFade(startedAt) {
    const t = new Date(startedAt).getTime();
    if (!t || Number.isNaN(t)) return 0.85;
    const ageDays = (Date.now() - t) / 86400000;
    return Math.max(0, Math.min(0.9, 0.9 * (1 - ageDays / tripFadeDays)));
}

// Marker urgency by HUE, normalised across the tasks currently shown: the
// freshest open task is blue, the one we're most behind on (longest without a
// reply) is red — the scale stretches to fit whatever's on screen, so the
// colours stay informative instead of all saturating yellow against a fixed
// deadline. Closed = grey, escalated (manual/AI) = hard red; neither joins the
// normalisation window (see applyAgeColors()).
const ESCALATED_COLOR = '#ef4444';

// HSL→hex so MapLibre paint expressions get a plain colour string.
function hslToHex(h, s, l) {
    s /= 100; l /= 100;
    const a = s * Math.min(l, 1 - l);
    const f = (n) => {
        const k = (n + h / 30) % 12;
        const c = l - a * Math.max(-1, Math.min(k - 3, 9 - k, 1));
        return Math.round(255 * c).toString(16).padStart(2, '0');
    };
    return `#${f(0)}${f(8)}${f(4)}`;
}

function taskAgeHours(task) {
    const dateStr = task.date || new Date().toISOString();
    return (Date.now() - new Date(dateStr).getTime()) / (1000 * 60 * 60);
}
function isClosedStatus(status) {
    const s = (status || '').toLowerCase();
    // `includes('closed')` also catches Zoho's "Auto closed" (and any
    // "… closed" variant), which an exact match missed — those used to render
    // as active on the map.
    return s === 'completed' || s === 'cancelled' || s.includes('closed');
}
function isEscalated(task) {
    return task.escalated === true || task.ai_priority === 'critical'
        || task.meta?.escalated === true || task.meta?.ai_priority === 'critical';
}

const DELIVERED_STATUSES = new Set(['delivered', 'zugestellt', 'ausgeliefert', 'geliefert']);
function isDeliveredStatus(status) {
    return DELIVERED_STATUSES.has((status || '').trim().toLowerCase());
}

// Parse ISO or German "DD.MM.YYYY[ - HH:MM Uhr]". Returns Date or null.
function parseShipDate(s) {
    if (!s || typeof s !== 'string') return null;
    const iso = Date.parse(s);
    if (!isNaN(iso)) return new Date(iso);
    const m = s.match(/^\s*(\d{1,2})\.(\d{1,2})\.(\d{4})(?:\s*-\s*(\d{1,2}):(\d{2}))?/);
    if (m) {
        const [_, d, mo, y, hh, mm] = m;
        return new Date(+y, +mo - 1, +d, +(hh || 0), +(mm || 0));
    }
    return null;
}

function shipmentDates(ship, raw) {
    const delivered = isDeliveredStatus(ship.status);
    const createdAt =
        parseShipDate(raw.created_at) ||
        (!delivered ? parseShipDate(raw.status_date) : null) ||
        parseShipDate(ship.updated_at);
    const deliveredAt = delivered
        ? (parseShipDate(raw.delivery_date) || parseShipDate(raw.status_date) || parseShipDate(ship.updated_at))
        : null;
    return { createdAt, deliveredAt, delivered };
}

// Office / Home location (Eschborn, Germany)
const HOME_LOCATION = { lat: 50.1407, lng: 8.5721, name: '9eck Central Warehouse (Eschborn)' };

// Base-map styles — MapLibre GL vector (OpenFreeMap, no API key). Same engine
// + style host as the Android MapLibre Native map, so mobile and desktop stay
// visually consistent.
const MAP_PROVIDERS = [
    { id: 'dark',     name: 'Dark',               style: 'https://tiles.openfreemap.org/styles/dark' },
    { id: 'bright',   name: 'Bright',             style: 'https://tiles.openfreemap.org/styles/bright' },
    { id: 'positron', name: 'Positron (Light)',   style: 'https://tiles.openfreemap.org/styles/positron' },
    { id: 'liberty',  name: 'Liberty (Detailed)', style: 'https://tiles.openfreemap.org/styles/liberty' },
];
let currentMapStyle = 'dark';
function styleUrl(id) {
    return (MAP_PROVIDERS.find(p => p.id === id) || MAP_PROVIDERS[0]).style;
}

// Distinct emojis used as marker icons (rendered to images — emoji don't render
// through the vector glyph fonts).
const EMOJI_SET = ['🔧', '💬', '📦', '⏸️', '⏳', '✅', '✖️', '🏢'];

// Geocoding cache backed by localStorage
const GEO_CACHE_KEY = 'eck_geo_cache';
const MAP_VIEW_KEY = 'eck_map_view';   // remembered {lng,lat,zoom} across reloads
const DAY_MS = 86_400_000;
let geoCache = {};

onMount(async () => {
    try { geoCache = JSON.parse(localStorage.getItem(GEO_CACHE_KEY) || '{}'); } catch { geoCache = {}; }
    currentMapStyle = 'dark';   // map is always dark (style switch removed)

    try {
        const [rmas, tickets, shipmentsRes, slaCfg] = await Promise.all([
            api.get('/api/rma?type=repair'),
            api.get('/api/support/tickets'),
            api.get('/api/delivery/shipments'),
            api.get('/api/admin/config/dashboard_sla').catch(() => null),
        ]);
        if (slaCfg) {
            if (typeof slaCfg.aging_scale_days === 'number') agingScaleDays = slaCfg.aging_scale_days;
            if (typeof slaCfg.repair_aging_scale_days === 'number') repairAgingScaleDays = slaCfg.repair_aging_scale_days;
            if (typeof slaCfg.trip_fade_days === 'number' && slaCfg.trip_fade_days > 0) tripFadeDays = slaCfg.trip_fade_days;
        }

        activeShipments = (shipmentsRes || []).filter(s => s.status !== 'cancelled');

        const RECENT_HOURS = 48;
        const mappedRepairs = (rmas || [])
            .filter(r => {
                if (r.status !== 'completed' && r.status !== 'cancelled') return true;
                const closedAt = new Date(r.updated_at || r.created_at).getTime();
                return ((Date.now() - closedAt) / 3600000) < RECENT_HOURS;
            })
            .map(r => ({
                _type: 'repair',
                id: r.id,
                reference: r.order_number || '—',
                customer_name: r.customer_name || 'Unknown',
                status: r.status,
                date: r.started_at || r.created_at,
                geo: r.metadata?.geo,
                addressInfo: { address: r.metadata?.address, zip: r.metadata?.zip, city: r.metadata?.city, country: r.metadata?.country || 'Germany' }
            }));

        const mappedTickets = (tickets || [])
            .filter(t => {
                if (!isClosedStatus(t.status)) return true;
                const closedAt = new Date(t.latest_update).getTime();
                return ((Date.now() - closedAt) / 3600000) < RECENT_HOURS;
            })
            .map(t => ({
                _type: 'ticket',
                id: t.ticket_id,
                reference: t.ticket_number || '—',
                customer_name: t.customer || t.company || 'Unknown',
                status: t.status,
                date: t.last_outbound_at || t.latest_update,
                geo: t.geo,
                assignee_id: t.assignee_id || '',
                assignee_name: t.assignee_name || '',
                addressInfo: { address: t.address, zip: t.zip, city: t.city, country: 'Germany' }
            }));

        activeTasks = [...mappedRepairs, ...mappedTickets];
        rebuildAssigneeChips();

        await initMap();

        // Popup buttons are raw HTML strings, so use window-scoped handlers.
        window.__eckResetToHQ = async function(table, cleanId) {
            try {
                const token = localStorage.getItem('auth_token');
                const res = await fetch('/api/geo/fix', {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${token}` },
                    body: JSON.stringify({ table, id: cleanId, mode: 'reset_home' }),
                });
                if (!res.ok) throw new Error((await res.text().catch(() => res.statusText)) || `HTTP ${res.status}`);
                const data = await res.json();
                const fid = `${table}:${cleanId}`;
                const feat = taskFeatures.find(f => f.properties.fid === fid);
                if (feat && typeof data.lat === 'number' && typeof data.lng === 'number') {
                    feat.geometry.coordinates = [data.lng, data.lat];
                    refreshTaskSource();
                }
                if (map) map._eckPopup?.remove();
                toastStore.add(tr('map.pin_moved_hq'), 'success');
            } catch (err) {
                console.error('reset-to-HQ failed', err);
                toastStore.add(tr('map.reset_failed', { error: err.message }), 'error');
            }
        };

        window.__eckPlanVisit = async function(lat, lng, reference, table, cleanId) {
            try {
                const token = localStorage.getItem('auth_token');
                const due = new Date().toISOString().slice(0, 10);
                const res = await fetch('/api/visits', {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${token}` },
                    body: JSON.stringify({
                        title: reference || 'Besuch', lat: parseFloat(lat), lng: parseFloat(lng),
                        due_date: due, target_entity_type: table, target_entity_id: cleanId,
                    }),
                });
                if (!res.ok) throw new Error(await res.text().catch(() => res.statusText));
                toastStore.add(tr('map.visit_planned', { ref: reference }), 'success');
            } catch (err) {
                console.error('plan-visit failed', err);
                toastStore.add(tr('map.plan_failed', { error: err.message }), 'error');
            }
        };

        window.__eckResolveShip = async function(id) {
            try {
                await api.post(`/api/delivery/shipments/${id}/resolve`, {});
                toastStore.add(tr('map.ship_delivered'), 'success');
                map._eckPopup?.remove();
                const fresh = await api.get('/api/delivery/shipments');
                activeShipments = (fresh || []).filter(s => s.status !== 'cancelled');
                await buildShipmentFeatures();
                if (map.getSource('ships')) map.getSource('ships').setData(fc(shipmentFeatures));
            } catch (e) {
                toastStore.add(tr('map.resolve_failed', { error: e.message || e }), 'error');
            }
        };

        // Select a row → fill the card pane NEXT TO the list. We update only the
        // card DOM (not a full re-render) so the list keeps its scroll position;
        // clicking another row just swaps the card.
        window.__eckTaskCard = function(fid, el) {
            if (!lastClusterLeaves) return;
            const leaf = lastClusterLeaves.find((l) => l.properties.fid === fid);
            const card = document.getElementById('eck-card');
            if (!leaf || !card) return;
            if (el && el.parentElement) {
                el.parentElement.querySelectorAll('.eck-row').forEach((r) => {
                    r.style.borderLeftColor = 'transparent';
                    r.style.background = 'transparent';
                });
                el.style.borderLeftColor = '#4a69bd';
                el.style.background = 'rgba(74,105,189,0.22)';
            }
            card.setAttribute('data-fid', fid);
            card.innerHTML = leaf.properties.popup + '<div id="eck-sum"></div>';
            card.style.display = '';
            card.scrollTop = 0;
            loadCardSummary(fid);
        };
        loadTripsOverlay();
        // Re-run every 5 min (only while this page is visible) so in-progress car
        // positions and the per-trip age fades stay current even without live WS
        // consent. Cleared in onDestroy.
        tripsInterval = setInterval(() => { if (active) loadTripsOverlay(); }, 5 * 60_000);
    } catch (e) {
        console.error('Failed to load dashboard data', e);
    } finally {
        loading = false;
    }
});

onDestroy(() => {
    if (tripsInterval) { clearInterval(tripsInterval); tripsInterval = null; }
    for (const t of liveTimers.values()) clearTimeout(t);
    liveTimers.clear();
    liveFeatures.clear();
    if (map) { map.remove(); map = null; }
    if (typeof window !== 'undefined') {
        delete window.__eckResetToHQ;
        delete window.__eckPlanVisit;
        delete window.__eckResolveShip;
        delete window.__eckTaskCard;
    }
});

function fc(features) { return { type: 'FeatureCollection', features }; }
// Render a UTC timestamp in the viewer's LOCAL time. The server emits UTC
// (`…+00:00`) and stores/seals in UTC (GoBD + Hedera need one DST-proof instant);
// only the DISPLAY is localised — the browser applies its own zone (Europe/Berlin
// here). Falls back to the raw slice if the string isn't a parseable instant.
function fmtLocal(iso, withTime = true) {
    if (!iso) return '';
    const d = new Date(iso);
    if (isNaN(d.getTime())) return String(iso).slice(0, 16).replace('T', ' ');
    return d.toLocaleString('de-DE', withTime
        ? { day: '2-digit', month: '2-digit', year: 'numeric', hour: '2-digit', minute: '2-digit' }
        : { day: '2-digit', month: '2-digit', year: 'numeric' });
}
// Great-circle distance in metres between two [lng,lat] points (the trip-line
// teleport gate works in metres, independent of latitude/zoom).
function metersBetween(a, b) {
    const R = 6371000, toR = Math.PI / 180;
    const dLat = (b[1] - a[1]) * toR, dLng = (b[0] - a[0]) * toR;
    const la1 = a[1] * toR, la2 = b[1] * toR;
    const h = Math.sin(dLat / 2) ** 2 + Math.cos(la1) * Math.cos(la2) * Math.sin(dLng / 2) ** 2;
    return 2 * R * Math.asin(Math.min(1, Math.sqrt(h)));
}
function escPlate(s) {
    return String(s).replace(/[&<>"']/g, c =>
        ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
}

// ── Assignee chips ─────────────────────────────────────────────────────────
function taskAssigneeKey(task) {
    if (task._type === 'repair') return REPAIRS_KEY;
    if (task.assignee_id) return `a:${task.assignee_id}`;
    if (task.assignee_name) return `n:${task.assignee_name}`;
    return UNASSIGNED_KEY;
}
function taskAssigneeLabel(task) {
    if (task._type === 'repair') return tr('map.assignee_repairs');
    return task.assignee_name || tr('map.assignee_unassigned');
}
function rebuildAssigneeChips() {
    const counts = new Map(), labels = new Map();
    for (const t of activeTasks) {
        const k = taskAssigneeKey(t);
        counts.set(k, (counts.get(k) || 0) + 1);
        if (!labels.has(k)) labels.set(k, taskAssigneeLabel(t));
    }
    const entries = Array.from(counts.entries()).map(([key, count]) => ({ key, label: labels.get(key), count }));
    entries.sort((a, b) => {
        const order = (k) => k === REPAIRS_KEY ? 0 : k === UNASSIGNED_KEY ? 2 : 1;
        const oa = order(a.key), ob = order(b.key);
        if (oa !== ob) return oa - ob;
        return a.label.localeCompare(b.label);
    });
    assigneeChips = entries;
    selectedAssignees = new Set(entries.map(e => e.key));
}
function toggleAssignee(key) {
    if (selectedAssignees.has(key)) selectedAssignees.delete(key);
    else selectedAssignees.add(key);
    selectedAssignees = new Set(selectedAssignees);
    refreshTaskSource();
}

// Tasks visible under the current assignee filter. Colours are normalised over
// exactly this shown set, so the blue↔red scale always reflects what's on map.
function filteredTaskFeatures() {
    let feats = taskFeatures.filter(f => selectedAssignees.has(f.properties.assigneeKey));
    const q = searchQuery.trim().toLowerCase();
    if (q) {
        feats = feats.filter(f =>
            (f.properties.ref || '').toLowerCase().includes(q) ||
            (f.properties.name || '').toLowerCase().includes(q));
    }
    return applyAgeColors(feats);
}
function refreshTaskSource() {
    if (map && map.getSource('tasks')) map.getSource('tasks').setData(fc(filteredTaskFeatures()));
}

// Re-filter the markers to the query and frame the matches on the map.
function runSearch() {
    refreshTaskSource();
    if (!map || !searchQuery.trim()) return;
    const feats = filteredTaskFeatures();
    if (feats.length === 0) return;
    if (feats.length === 1) {
        map.flyTo({ center: feats[0].geometry.coordinates, zoom: 13 });
        return;
    }
    let minLng = Infinity, minLat = Infinity, maxLng = -Infinity, maxLat = -Infinity;
    for (const f of feats) {
        const [lng, lat] = f.geometry.coordinates;
        minLng = Math.min(minLng, lng); maxLng = Math.max(maxLng, lng);
        minLat = Math.min(minLat, lat); maxLat = Math.max(maxLat, lat);
    }
    map.fitBounds([[minLng, minLat], [maxLng, maxLat]], { padding: 60, maxZoom: 13 });
}

// Voice input: dictate the search query (Web Speech API, de-DE).
function dictateSearch() {
    const SR = window.SpeechRecognition || window.webkitSpeechRecognition;
    if (!SR) { toastStore.add(tr('map.speech_unavailable'), 'error'); return; }
    const rec = new SR();
    rec.lang = 'de-DE';
    rec.interimResults = false;
    rec.maxAlternatives = 1;
    listening = true;
    rec.onresult = (e) => { searchQuery = e.results[0][0].transcript; runSearch(); };
    rec.onerror = () => { listening = false; };
    rec.onend = () => { listening = false; };
    try { rec.start(); } catch { listening = false; }
}
function clearSearch() { searchQuery = ''; refreshTaskSource(); }

// ── Geocoding (server-side, cached) ─────────────────────────────────────────
// Privacy: the browser NEVER calls Nominatim. It asks our server (zip+city
// only, never a street), which resolves once + caches in the DB. A thin
// localStorage cache (keyed by zip|city) avoids re-asking within a session.
async function serverResolve(zip, city) {
    const z = (zip || '').trim();
    const c = (city || '').trim();
    if (!z && !c) return null;
    const key = `${z}|${c}`.toLowerCase();
    if (geoCache[key] !== undefined) return geoCache[key];
    try {
        const qs = new URLSearchParams();
        if (z) qs.set('zip', z);
        if (c) qs.set('city', c);
        const r = await api.get(`/api/geo/resolve?${qs.toString()}`);
        const coords = (r && typeof r.lat === 'number') ? { lat: r.lat, lng: r.lng } : null;
        geoCache[key] = coords;
        localStorage.setItem(GEO_CACHE_KEY, JSON.stringify(geoCache));
        return coords;
    } catch (e) {
        console.warn('geo resolve failed for', key, e);
        return null;
    }
}

// ── Urgency + visual derivation ────────────────────────────────────────────
// Age-window colouring lives in applyAgeColors() (filter-aware, runs over the
// shown set). taskVisual derives only the age-independent bits: emoji, opacity,
// and the closed/escalated flags + raw age (carried on the feature).
function taskVisual(task) {
    const hours = taskAgeHours(task);
    const escalated = isEscalated(task);
    const closed = isClosedStatus(task.status);
    let emoji = task._type === 'ticket' ? '💬' : '🔧';
    let opacity = task._type === 'ticket' ? 0.8 : 1.0;
    const status = (task.status || '').toLowerCase();
    if (closed) {
        emoji = status === 'cancelled' ? '✖️' : '✅';
        opacity = Math.max(0.1, opacity - (hours / 48));
    } else {
        if (status === 'waiting_parts' || status === 'received') emoji = '📦';
        else if (status === 'processing') emoji = '🔧';
        else if (status === 'on_hold' || status === 'onhold') emoji = '⏸️';
        else if (status === 'pending' || status === 'open') emoji = '⏳';
    }
    return { emoji, opacity, escalated, closed, ageHours: hours };
}

// Colour each shown marker's ring by how overdue it is, normalised across the
// passed (currently shown / filtered) set: freshest open → blue (hue 240),
// most-overdue open → red (hue 0). Closed = grey, escalated = hard red; both
// sit outside the window so they don't skew it. Fills in color + score.
function applyAgeColors(features) {
    let lo = Infinity, hi = -Infinity;
    for (const f of features) {
        const p = f.properties;
        if (p.closed || p.escalated) continue;
        if (p.ageHours < lo) lo = p.ageHours;
        if (p.ageHours > hi) hi = p.ageHours;
    }
    const span = hi - lo;
    return features.map((f) => {
        const p = f.properties;
        let color, score;
        if (p.closed) { color = '#6b7280'; score = -1; }   // -1 = no open work → grey
        else if (p.escalated) { color = ESCALATED_COLOR; score = 2; }
        else {
            const t = span > 0 ? (p.ageHours - lo) / span : 0.5; // no spread → neutral
            color = hslToHex(Math.round(240 * (1 - t)), 85, 55);
            score = t;
        }
        return { ...f, properties: { ...p, color, score } };
    });
}

function taskPopupHtml(task, lat, lng, cleanId, backendTable) {
    const linkUrl = task._type === 'repair'
        ? `${base}/dashboard/repairs/${cleanId}` : `${base}/dashboard/support/${cleanId}`;
    const btnText = task._type === 'repair' ? tr('map.open_repair') : tr('map.open_ticket');
    const typeLabel = task._type === 'repair' ? tr('map.type_repair') : tr('map.type_ticket');
    const assigneeLine = task._type === 'ticket' && task.assignee_name
        ? `<div style="color:#888;font-size:0.8em;margin-top:2px">👤 ${task.assignee_name}</div>` : '';
    return `<div style="font-family:var(--font-family);min-width:150px">
        <div style="font-size:0.7em;color:#a855f7;text-transform:uppercase;margin-bottom:2px;font-weight:bold;">${typeLabel}</div>
        <strong style="color:#fff">${task.reference}</strong><br/>
        <span style="color:#bbb">${task.customer_name}</span><br/>
        <span style="color:#bbb;font-size:0.85em">${tr('map.status_label')} <b style="color:#ddd">${task.status}</b></span>
        ${assigneeLine}
        <div style="display:flex;gap:4px;margin-top:8px;flex-wrap:wrap">
            <a href="${linkUrl}" style="background:#4a69bd;color:#fff;padding:4px 8px;text-decoration:none;border-radius:4px;font-size:0.85em">${btnText}</a>
            <button type="button" onclick="window.__eckResetToHQ &amp;&amp; window.__eckResetToHQ('${backendTable}','${cleanId}')"
               style="background:#2a2a2a;color:#fbbf24;border:1px solid #f59e0b;padding:4px 8px;border-radius:4px;font-size:0.85em;cursor:pointer">${tr('map.reset_hq')}</button>
            <button type="button" onclick="window.__eckPlanVisit &amp;&amp; window.__eckPlanVisit('${lat}','${lng}','${(task.reference || '').replace(/'/g, '')}','${backendTable}','${cleanId}')"
               style="background:#1e3a8a;color:#93c5fd;border:1px solid #3b82f6;padding:4px 8px;border-radius:4px;font-size:0.85em;cursor:pointer">${tr('map.plan_visit')}</button>
        </div></div>`;
}

// Build task point features (geocoding missing coords). Sets feature props for
// the data-driven layers + a precomputed popup HTML string.
async function buildTaskFeatures() {
    const out = [];
    for (const task of activeTasks) {
        let lat = task.geo?.lat, lng = task.geo?.lng;
        // Prefer the server-resolved geo (the geocoder worker persists it). Only
        // fall back to an on-demand resolve by zip+city — never the street, and
        // never from the browser directly.
        if (!lat || !lng) {
            const z = (task.addressInfo?.zip || '').trim();
            const c = (task.addressInfo?.city || '').trim();
            if (z || c) {
                const coords = await serverResolve(z, c);
                if (coords) { lat = coords.lat; lng = coords.lng; }
            }
        }
        if (!lat || !lng) continue;

        const v = taskVisual(task);
        const rawId = task.id?.id || task.id;
        const cleanId = typeof rawId === 'string' && rawId.includes(':') ? rawId.split(':')[1] : rawId;
        const backendTable = task._type === 'repair' ? 'order' : 'document';
        const linkUrl = task._type === 'repair'
            ? `${base}/dashboard/repairs/${cleanId}` : `${base}/dashboard/support/${cleanId}`;

        out.push({
            type: 'Feature',
            geometry: { type: 'Point', coordinates: [lng, lat] },
            properties: {
                fid: `${backendTable}:${cleanId}`,
                assigneeKey: taskAssigneeKey(task),
                icon: `em-${v.emoji}`,
                opacity: v.opacity,
                ageHours: v.ageHours,
                escalated: v.escalated,
                closed: v.closed,
                ref: task.reference || cleanId || '—',   // ticket/order number
                name: task.customer_name || '',          // company / customer for the list row
                href: linkUrl,                            // open-link for the cluster list
                // color + score are filled per-render by applyAgeColors()
                popup: taskPopupHtml(task, lat, lng, cleanId, backendTable),
            },
        });
    }
    taskFeatures = out;
}

async function buildShipmentFeatures() {
    const out = [];
    const now = Date.now();
    for (const ship of activeShipments) {
        try {
            const raw = JSON.parse(ship.raw_response || '{}');
            const targetCity = raw.delivery_city || raw.recipient_city;
            if (!targetCity) continue;
            const { createdAt, deliveredAt, delivered } = shipmentDates(ship, raw);
            const deliveredAgeDays = deliveredAt ? (now - deliveredAt.getTime()) / DAY_MS : null;
            const createdAgeDays = createdAt ? (now - createdAt.getTime()) / DAY_MS : null;
            if (delivered && deliveredAgeDays !== null && deliveredAgeDays >= SHIP_HIDE_DELIVERED_AFTER_DAYS) continue;
            const coords = await serverResolve(null, targetCity);
            if (!coords) continue;
            const isDHL = ship.provider === 'dhl';
            const isStuck = !delivered && createdAgeDays !== null && createdAgeDays > SHIP_ALERT_UNDELIVERED_AFTER_DAYS;
            const color = delivered ? '#6b7280' : isStuck ? '#ef4444' : (isDHL ? '#facc15' : '#22c55e');
            const opacity = delivered ? 0.3 : (isStuck ? 0.9 : 0.5);
            const width = isStuck ? 4 : 2;
            const shipId = typeof ship.id === 'string' && ship.id.includes(':') ? ship.id.split(':')[1] : (ship.id?.id || ship.id);
            const popup = isStuck
                ? `<div style="font-family:var(--font-family);min-width:180px">
                     <div style="color:#ef4444;font-weight:700;font-size:0.85em;margin-bottom:4px">${tr('map.stuck_days', { days: Math.floor(createdAgeDays) })}</div>
                     <strong style="color:#fff">${ship.provider.toUpperCase()} · ${ship.tracking_number}</strong><br/>
                     <span style="color:#bbb;font-size:0.8em">${tr('map.carrier_status')} <b style="color:#ddd">${ship.status}</b></span>
                     <div style="color:#888;font-size:0.75em;margin-top:4px">${tr('map.carrier_glitch')}</div>
                     <button type="button" onclick="window.__eckResolveShip &amp;&amp; window.__eckResolveShip('${shipId}')"
                        style="margin-top:8px;background:#ef4444;color:#fff;border:none;padding:5px 10px;border-radius:4px;font-size:0.8em;cursor:pointer">${tr('map.mark_delivered')}</button>
                   </div>`
                : `<div style="font-family:var(--font-family)">📦 ${ship.provider.toUpperCase()} · ${ship.tracking_number}</div>`;
            out.push({
                type: 'Feature',
                geometry: { type: 'LineString', coordinates: [[HOME_LOCATION.lng, HOME_LOCATION.lat], [coords.lng, coords.lat]] },
                properties: { color, opacity, width, popup },
            });
        } catch (e) { console.warn('shipment build failed', e); }
    }
    shipmentFeatures = out;
}

// ── Trips overlay (historical) ─────────────────────────────────────────────

// Build the polyline coordinates ([lng,lat] pairs) for one historical trip.
//   • Preferred: the client's own `smoothed_path` — it already fused GPS + cell
//     and map-matched away the tower noise, is in GeoJSON order and decimated,
//     so we trust it verbatim rather than re-deriving a worse line here.
//   • Legacy trips (recorded before the client emitted smoothed_path): rebuild a
//     sane line from the raw points, because those mix precise fused/gps fixes
//     with cell resolutions carrying 0.7–3 km (occasionally 100+ km) of error,
//     which makes a naive "connect every point" line zig-zag across the map.
function tripLineCoords(full) {
    // Road-snapped geometry beats the smoothed one when the client sent both.
    const mp = full?.matched_path;
    if (Array.isArray(mp) && mp.length >= 2) return mp;
    const sp = full?.smoothed_path;
    if (Array.isArray(sp) && sp.length >= 2) return sp;

    // Located points only, ordered by time (fixes can arrive/persist out of seq
    // order); seq is the tiebreaker and the fallback when a ts is missing.
    const raw = (full?.points || [])
        .filter(p => typeof p.lat === 'number' && typeof p.lng === 'number')
        .sort((a, b) => {
            const ta = Date.parse(a.ts), tb = Date.parse(b.ts);
            if (!Number.isNaN(ta) && !Number.isNaN(tb) && ta !== tb) return ta - tb;
            return (a.seq || 0) - (b.seq || 0);
        });
    if (raw.length < 2) return [];

    // Prefer precision: when there's a real GPS/fused backbone we draw ONLY that,
    // and let cell fixes back in solely to bridge a > 120 s hole in the backbone
    // (where the phone had no satellite fix) — otherwise the line would jump
    // straight across the gap. Without any precise backbone, cell is all we have.
    const isPrecise = p => p.source === 'fused' || p.source === 'gps';
    const precise = raw.filter(isPrecise);
    let seq;
    if (precise.length >= 2) {
        seq = [];
        for (let i = 0; i < precise.length; i++) {
            seq.push(precise[i]);
            const cur = precise[i], nxt = precise[i + 1];
            if (!nxt) continue;
            const tc = Date.parse(cur.ts), tn = Date.parse(nxt.ts);
            if (Number.isNaN(tc) || Number.isNaN(tn) || (tn - tc) <= 120000) continue;
            for (const p of raw) {
                if (isPrecise(p)) continue;
                const tp = Date.parse(p.ts);
                if (!Number.isNaN(tp) && tp > tc && tp < tn) seq.push(p);
            }
        }
    } else {
        seq = raw;
    }

    // Speed gate: drop a point only when it teleports from BOTH neighbours (a lone
    // bad-tower fix), so a single wild point is removed but a genuinely fast leg
    // between two good fixes survives. A pair with a missing ts can't be judged →
    // treat it as plausible (skip the gate) rather than crash on NaN.
    const teleports = (a, b) => {
        if (!a || !b) return false;
        const ta = Date.parse(a.ts), tb = Date.parse(b.ts);
        if (Number.isNaN(ta) || Number.isNaN(tb)) return false;
        const dt = Math.abs(tb - ta) / 1000;
        if (dt === 0) return false;
        const kmh = metersBetween([a.lng, a.lat], [b.lng, b.lat]) / dt * 3.6;
        return kmh > 250;
    };
    const kept = seq.filter((p, i) =>
        !(teleports(seq[i - 1], p) && teleports(p, seq[i + 1])));

    const coords = kept.map(p => [p.lng, p.lat]);
    return coords.length >= 2 ? coords : [];
}

// Normalise a trip id/uuid to its bare uuid (strips a "trip:" table prefix and
// unwraps a SurrealDB {tb,id} record id), so progress-car features and WS
// TRIP_LIVE messages can be matched to the same trip.
function tripUuidOf(v) {
    if (v == null) return '';
    if (typeof v === 'object') v = v.id ?? '';
    v = String(v);
    const i = v.indexOf(':');
    return i >= 0 ? v.slice(i + 1) : v;
}

// Integer km, German-grouped (45230 → "45.230").
function fmtKm(v) { return Math.round(v).toLocaleString('de-DE'); }

// One popup per trip — shared by the line, the START/FINISH flags and the
// in-progress car. Shows BOTH ends at once (start + finish rows), the distance
// estimate (+ road/smoothed hint) and the GoBD seal. A trip still running shows
// the finish row as the "in progress" state instead of an end time/odometer.
function tripPopupHtml(full) {
    const head = full.vehicle_plate ? escPlate(full.vehicle_plate) + ' · ' : '';
    const startOdo = full.start_odometer_km != null ? ` · ${fmtKm(full.start_odometer_km)} km` : '';
    const startRow = `<div style="color:#bbb;font-size:0.85em">▶ ${tr('map.start')} ${fmtLocal(full.started_at)}${startOdo}</div>`;
    let finishRow;
    if (full.ended_at) {
        const endOdo = full.end_odometer_km != null ? ` · ${fmtKm(full.end_odometer_km)} km` : '';
        finishRow = `<div style="color:#bbb;font-size:0.85em">🏁 ${tr('map.finish')} ${fmtLocal(full.ended_at)}${endOdo}</div>`;
    } else {
        finishRow = `<div style="color:#22c55e;font-size:0.85em">🏁 ${tr('map.in_progress')}</div>`;
    }
    // Distance: the REAL mileage (odometer delta) leads whenever both readings
    // exist; the GPS estimate always rides along so driver/boss/Finanzamt see at
    // a glance that the two agree (mirrors the export's Km (Tacho) vs geschätzt —
    // no point fudging a number that's cross-checked right next to it). With an
    // incomplete odometer pair only the estimate remains. The smoothed/matched
    // debug hints are gone — that's pipeline internals, not client info.
    const so = full.start_odometer_km, eo = full.end_odometer_km;
    const realKm = (so != null && eo != null && eo >= so) ? Math.round(eo - so) : null;
    const est = full.computed_distance_km != null ? tr('map.km_estimated', { km: full.computed_distance_km }) : '';
    const km = realKm != null
        ? tr('map.km_actual', { km: fmtKm(realKm) }) + (est ? ` · ${est}` : '')
        : est;
    const sealed = full.seal_hash ? tr('map.gobd_sealed') : '';
    return `<div style="font-family:var(--font-family)">
        <strong style="color:#fff">${head}${tr('map.trip')}</strong><br/>
        ${startRow}${finishRow}
        <span style="color:#bbb;font-size:0.85em">${km}</span><br/>
        <span style="color:#22c55e;font-size:0.8em">${sealed}</span></div>`;
}

async function loadTripsOverlay() {
    try {
        const trips = await api.get('/api/trips?limit=20');
        if (!Array.isArray(trips) || !map) return;
        const lines = [], marks = [], progress = [];
        for (const t of trips) {
            const full = await api.get(`/api/trips/${t.id}`).catch(() => null);
            const pts = tripLineCoords(full);
            if (pts.length < 2) continue;
            // Per-vehicle colour + age fade (to nothing over tripFadeDays). A
            // fully-faded trip is dropped so the map doesn't accumulate history.
            const opacity = tripFade(full.started_at);
            if (opacity <= 0.02) continue;
            const color = plateColor(full.vehicle_plate);
            const popup = tripPopupHtml(full);
            lines.push({ type: 'Feature', geometry: { type: 'LineString', coordinates: pts }, properties: { popup, color, opacity } });

            const startPt = pts[0], endPt = pts[pts.length - 1];
            // Every trip gets a START flag at its first point.
            marks.push({ type: 'Feature', geometry: { type: 'Point', coordinates: startPt }, properties: { kind: 'start', popup, color } });
            if (full.ended_at) {
                // Completed → a FINISH flag at the last point. At a leg junction the
                // finish of leg N and the start of leg N+1 share one point; the two
                // banners splay left/right around it and both stay visible.
                marks.push({ type: 'Feature', geometry: { type: 'Point', coordinates: endPt }, properties: { kind: 'finish', popup, color } });
            } else {
                // In progress → a car at the last position (NO finish flag) until a
                // WS TRIP_LIVE for this trip takes over (handleTripLive drops it).
                progress.push({ type: 'Feature', geometry: { type: 'Point', coordinates: endPt },
                    properties: { plate: full.vehicle_plate || '', popup, tripUuid: tripUuidOf(t.id) } });
            }
        }
        tripFeatures = lines;
        tripMarkFeatures = marks;
        // Don't place a static car for a trip the live layer already owns.
        const liveBare = new Set([...liveFeatures.keys()].map(tripUuidOf));
        progressFeatures = progress.filter(f => !liveBare.has(f.properties.tripUuid));
        if (map.getSource('trips')) map.getSource('trips').setData(fc(tripFeatures));
        if (map.getSource('trip-marks')) map.getSource('trip-marks').setData(fc(tripMarkFeatures));
        if (map.getSource('progress')) map.getSource('progress').setData(fc(progressFeatures));
    } catch (e) {
        console.warn('trips overlay failed', e);
    }
}

function toggleTrips() {
    showTrips = !showTrips;
    const v = showTrips ? 'visible' : 'none';
    for (const id of ['trips-line', 'trip-marks', 'progress']) {
        if (map && map.getLayer(id)) map.setLayoutProperty(id, 'visibility', v);
    }
}

// ── Live trips (consent-gated TRIP_LIVE) ───────────────────────────────────
function handleTripLive(msg) {
    if (!msg || msg.type !== 'TRIP_LIVE' || !map) return;
    if (Date.now() - (msg._receivedAt || 0) > 5000) return;
    const id = msg.trip_uuid;
    if (!id || typeof msg.lat !== 'number' || typeof msg.lng !== 'number') return;
    const bare = tripUuidOf(id);
    // The live layer now owns this trip → remove its static in-progress car (if the
    // overlay had placed one). Inherit that car's trip popup so the live marker is
    // clickable immediately, before extendLiveTrace refreshes it from the record.
    const priorPopup = progressFeatures.find(f => f.properties.tripUuid === bare)?.properties.popup
        || liveFeatures.get(id)?.properties.popup || '';
    if (progressFeatures.some(f => f.properties.tripUuid === bare)) {
        progressFeatures = progressFeatures.filter(f => f.properties.tripUuid !== bare);
        if (map.getSource('progress')) map.getSource('progress').setData(fc(progressFeatures));
    }
    liveFeatures.set(id, {
        type: 'Feature',
        geometry: { type: 'Point', coordinates: [msg.lng, msg.lat] },
        properties: {
            plate: msg.vehicle_plate || '',
            heading: (typeof msg.heading === 'number' && !Number.isNaN(msg.heading)) ? msg.heading : 0,
            popup: priorPopup,
        },
    });
    if (map.getSource('live')) map.getSource('live').setData(fc([...liveFeatures.values()]));
    // Extend the trace line up to this checkpoint (fetches the trip's accumulated points).
    extendLiveTrace(id);
    if (liveTimers.has(id)) clearTimeout(liveTimers.get(id));
    liveTimers.set(id, setTimeout(() => {
        liveFeatures.delete(id);
        liveTimers.delete(id);
        liveLineFeatures.delete(id);
        if (map && map.getSource('live')) map.getSource('live').setData(fc([...liveFeatures.values()]));
        if (map && map.getSource('live-trips')) map.getSource('live-trips').setData(fc([...liveLineFeatures.values()]));
    }, LIVE_STALE_MS));
}

// Fetch the in-progress trip's accumulated (server-persisted) points and (re)draw
// its trace line up to the latest checkpoint. Debounced per trip so a burst of
// TRIP_LIVE events triggers a single fetch. The open-checkpoint upload persists
// the whole track, so the line grows a segment each checkpoint.
async function extendLiveTrace(id) {
    if (liveLineFetching.has(id)) return;
    liveLineFetching.add(id);
    try {
        const full = await api.get(`/api/trips/${id}`).catch(() => null);
        // Give the live car a full trip popup (start / finish / in-progress) now
        // that we have the record — a WS TRIP_LIVE alone carries no odometer/seal.
        if (full) {
            const lf = liveFeatures.get(id);
            if (lf) {
                lf.properties.popup = tripPopupHtml(full);
                if (map && map.getSource('live')) map.getSource('live').setData(fc([...liveFeatures.values()]));
            }
        }
        const pts = (full?.points || [])
            .filter(p => typeof p.lat === 'number' && typeof p.lng === 'number')
            .sort((a, b) => (a.seq || 0) - (b.seq || 0))
            .map(p => [p.lng, p.lat]);
        if (pts.length >= 2) {
            liveLineFeatures.set(id, {
                type: 'Feature',
                geometry: { type: 'LineString', coordinates: pts },
                properties: { plate: full?.vehicle_plate || '', color: plateColor(full?.vehicle_plate) },
            });
            if (map && map.getSource('live-trips')) {
                map.getSource('live-trips').setData(fc([...liveLineFeatures.values()]));
            }
        }
    } finally {
        liveLineFetching.delete(id);
    }
}
// Only process live-car/trip WS updates while we're the visible page — no point
// mutating a hidden (display:none) map. Resumes on the next message when shown.
$: if (active && $wsStore.lastMessage) handleTripLive($wsStore.lastMessage);

// ── Map setup ──────────────────────────────────────────────────────────────
function handleStyleChange() {
    localStorage.setItem('eck_map_style', currentMapStyle);
    if (!map) return;
    map.setStyle(styleUrl(currentMapStyle));
    map.once('style.load', addImagesAndLayers); // setStyle wipes custom sources/layers
}

// ── Trip-flag geometry (shared by the icon draw, the layer icon-offset, and the
// per-pixel click hit-test — one source of truth for all three) ──────────────
// The flag icons are 48×48 canvases added at pixelRatio 2, so their DISPLAY size
// is 48/2 = 24 px (centre x = 12). icon-anchor 'bottom' pins the canvas
// bottom-CENTRE to the trip endpoint; icon-offset (unscaled display px, later
// ×icon-size) then slides the POLE-BASE x onto that point. A pole at canvas
// x = poleX is at display x = poleX/2, so offsetX = 12 − poleX/2 (independent of
// icon-size, so FLAG_ICON_SIZE can be tuned without touching the offsets).
const FLAG_SIZE = 48;               // canvas px per flag icon (added at pixelRatio 2)
const FLAG_ICON_SIZE = 0.7;         // MapLibre icon-size for the trip-marks layer
const FLAG_START_POLE_X = 34;       // START pole centre x (green banner extends LEFT)
const FLAG_FINISH_POLE_X = 14;      // FINISH pole centre x (chequered banner extends RIGHT)
const FLAG_START_OFF_X = 12 - FLAG_START_POLE_X / 2;    // = -5
const FLAG_FINISH_OFF_X = 12 - FLAG_FINISH_POLE_X / 2;  // = +5
// { 'flag-start' | 'flag-finish' → { data, size, anchorX, anchorY } } — the icon
// ImageData kept for flagHitAlpha(); anchor = the pole-base pixel (poleX, bottom).
const iconPixels = {};

// Render an emoji (or simple car) to an ImageData for use as a symbol icon.
function emojiImage(emoji, size = 48) {
    const c = document.createElement('canvas');
    c.width = c.height = size;
    const x = c.getContext('2d');
    x.font = `${Math.floor(size * 0.78)}px serif`;
    x.textAlign = 'center';
    x.textBaseline = 'middle';
    x.fillText(emoji, size / 2, size / 2);
    return x.getImageData(0, 0, size, size);
}
function carImage(fill, stroke, size = 48) {
    const c = document.createElement('canvas');
    c.width = c.height = size;
    const x = c.getContext('2d');
    const s = size / 48;
    x.translate(size / 2, size / 2);
    x.scale(s, s);
    x.translate(-16, -24);
    x.fillStyle = fill; x.strokeStyle = stroke; x.lineWidth = 1.5;
    x.beginPath();
    if (x.roundRect) x.roundRect(6, 4, 20, 40, 8); else x.rect(6, 4, 20, 40);
    x.fill(); x.stroke();
    x.fillStyle = stroke; x.globalAlpha = 0.55;
    if (x.roundRect) { x.beginPath(); x.roundRect(9, 9, 14, 9, 3); x.fill(); }
    return x.getImageData(0, 0, size, size);
}
// Canvas-drawn trip flags (48×48, added at pixelRatio 2). START = a solid green
// pennant whose banner extends LEFT of a right-hand pole; FINISH = a black/white
// chequered banner extending RIGHT of a left-hand pole. The pole runs to the
// canvas bottom so icon-anchor 'bottom' + the FLAG_*_OFF_X offset land the POLE
// BASE exactly on the trip endpoint. Fills are solid (only the triangle's
// diagonal antialiases, which the ≥32 alpha threshold in flagHitAlpha absorbs) —
// so the alpha channel doubles as the click mask, cached here in iconPixels.
function flagImage(kind) {
    const size = FLAG_SIZE;
    const c = document.createElement('canvas');
    c.width = c.height = size;
    const x = c.getContext('2d');
    const poleX = kind === 'finish' ? FLAG_FINISH_POLE_X : FLAG_START_POLE_X;
    // Pole (light grey, legible on the dark map) — drawn down to the very bottom.
    x.fillStyle = '#e5e7eb';
    x.fillRect(poleX - 1.5, 3, 3, size - 3);
    if (kind === 'finish') {
        // Chequered banner to the RIGHT of the pole: 3×2 cells + a thin border.
        const bx = poleX + 1.5, by = 4, bw = 27, bh = 18, cols = 3, rows = 2;
        const cw = bw / cols, ch = bh / rows;
        x.fillStyle = '#ffffff'; x.fillRect(bx, by, bw, bh);
        x.fillStyle = '#000000';
        for (let r = 0; r < rows; r++)
            for (let k = 0; k < cols; k++)
                if ((r + k) % 2 === 0) x.fillRect(bx + k * cw, by + r * ch, cw, ch);
        x.strokeStyle = '#000000'; x.lineWidth = 1;
        x.strokeRect(bx + 0.5, by + 0.5, bw - 1, bh - 1);
    } else {
        // Solid green triangular pennant to the LEFT of the pole.
        x.fillStyle = '#22c55e';
        x.beginPath();
        x.moveTo(poleX - 1.5, 4);
        x.lineTo(poleX - 1.5, 22);
        x.lineTo(6, 13);
        x.closePath();
        x.fill();
    }
    const img = x.getImageData(0, 0, size, size);
    // Cache for the hit-test: anchor = pole base = (poleX, bottom edge = size).
    iconPixels[kind === 'finish' ? 'flag-finish' : 'flag-start'] =
        { data: img.data, size, anchorX: poleX, anchorY: size };
    return img;
}

function addImagesAndLayers() {
    if (!map) return;
    // Icon images (emoji, live car, trip flags). Guard against re-adds after setStyle.
    for (const e of EMOJI_SET) {
        const id = `em-${e}`;
        if (!map.hasImage(id)) map.addImage(id, emojiImage(e, 48), { pixelRatio: 2 });
    }
    if (!map.hasImage('car-live')) map.addImage('car-live', carImage('#22c55e', '#0b3d1a'), { pixelRatio: 2 });
    if (!map.hasImage('flag-start')) map.addImage('flag-start', flagImage('start'), { pixelRatio: 2 });
    if (!map.hasImage('flag-finish')) map.addImage('flag-finish', flagImage('finish'), { pixelRatio: 2 });

    const ringColor = ['case',
        ['>=', ['get', 'maxScore'], 2], ESCALATED_COLOR,        // any escalated → red
        ['<', ['get', 'maxScore'], 0], '#6b7280',                // all closed → grey
        ['interpolate', ['linear'], ['get', 'maxScore'],
            0, '#3b82f6', 0.5, '#22c55e', 0.75, '#fbbf24', 1, '#ef4444']];

    // ── Sources ──────────────────────────────────────────────────────────────
    // Added up-front; the on-screen z-order is decided PURELY by the addLayer
    // sequence that follows.
    if (!map.getSource('home')) {
        map.addSource('home', { type: 'geojson', data: fc([{ type: 'Feature',
            geometry: { type: 'Point', coordinates: [HOME_LOCATION.lng, HOME_LOCATION.lat] }, properties: {} }]) });
    }
    if (!map.getSource('ships')) map.addSource('ships', { type: 'geojson', data: fc(shipmentFeatures) });
    else map.getSource('ships').setData(fc(shipmentFeatures));
    if (!map.getSource('trips')) map.addSource('trips', { type: 'geojson', data: fc(tripFeatures) });
    else map.getSource('trips').setData(fc(tripFeatures));
    if (!map.getSource('live-trips')) map.addSource('live-trips', { type: 'geojson', data: fc([...liveLineFeatures.values()]) });
    else map.getSource('live-trips').setData(fc([...liveLineFeatures.values()]));
    if (!map.getSource('tasks')) {
        map.addSource('tasks', {
            type: 'geojson', data: fc(filteredTaskFeatures()),
            // maxzoom MUST be > clusterMaxZoom or the cluster index is malformed
            // and getClusterLeaves returns nothing (clusters render but clicking
            // them does nothing). Keep clusterMaxZoom high (≥ map maxZoom 22) so
            // points sharing one exact coord stay a clickable cluster at every zoom.
            cluster: true, clusterRadius: 50, clusterMaxZoom: 22, maxzoom: 24,
            clusterProperties: { maxScore: ['max', ['get', 'score']] },
        });
    } else {
        map.getSource('tasks').setData(fc(filteredTaskFeatures()));
    }
    if (!map.getSource('trip-marks')) map.addSource('trip-marks', { type: 'geojson', data: fc(tripMarkFeatures) });
    else map.getSource('trip-marks').setData(fc(tripMarkFeatures));
    if (!map.getSource('progress')) map.addSource('progress', { type: 'geojson', data: fc(progressFeatures) });
    else map.getSource('progress').setData(fc(progressFeatures));
    if (!map.getSource('live')) map.addSource('live', { type: 'geojson', data: fc([...liveFeatures.values()]) });
    else map.getSource('live').setData(fc([...liveFeatures.values()]));

    // ── Layers, bottom → top ─────────────────────────────────────────────────
    // home · ships-line · trips-line · live-trips-line · clusters · pt-ring ·
    // pt-emoji · trip-marks (flags) · cluster-count · progress · live-cars.
    // Trip/ship LINES sit BELOW the client circles (a line never steals a circle's
    // click); the flags sit ABOVE the circles but BELOW cluster-count (so a
    // cluster's number stays readable through a flag); the cars stay on top.

    // Home — beneath the task layers. Tickets reset-to-HQ pile on this exact
    // coordinate; if 🏢 were on top it would hide their cluster count + rings.
    map.addLayer({ id: 'home', type: 'symbol', source: 'home',
        layout: { 'icon-image': 'em-🏢', 'icon-size': 0.55, 'icon-allow-overlap': true } });

    // Shipment routes
    map.addLayer({
        id: 'ships-line', type: 'line', source: 'ships',
        layout: { 'line-cap': 'round', 'line-join': 'round' },
        paint: { 'line-color': ['get', 'color'], 'line-width': ['get', 'width'],
            'line-opacity': ['get', 'opacity'], 'line-dasharray': [2, 2] },
    });

    // Historical trips (per-vehicle colour + age fade; see plateColor / tripFade).
    map.addLayer({
        id: 'trips-line', type: 'line', source: 'trips',
        layout: { 'line-cap': 'round', visibility: showTrips ? 'visible' : 'none' },
        paint: { 'line-color': ['get', 'color'], 'line-width': 3,
            'line-opacity': ['get', 'opacity'], 'line-dasharray': [2, 2] },
    });

    // Live trace: the growing path of in-progress trips (per-vehicle colour,
    // full opacity). Solid (not dashed) to distinguish from the historical trips.
    map.addLayer({
        id: 'live-trips-line', type: 'line', source: 'live-trips',
        layout: { 'line-cap': 'round', 'line-join': 'round' },
        paint: { 'line-color': ['coalesce', ['get', 'color'], '#22c55e'], 'line-width': 4, 'line-opacity': 0.95 },
    });

    // Tasks (clustered) — the client circles sit ABOVE the lines.
    map.addLayer({
        id: 'clusters', type: 'circle', source: 'tasks', filter: ['has', 'point_count'],
        paint: {
            'circle-color': '#1e1e1e',
            'circle-radius': ['step', ['get', 'point_count'], 16, 10, 20, 50, 26],
            'circle-stroke-width': 3, 'circle-stroke-color': ringColor,
        },
    });
    map.addLayer({
        id: 'pt-ring', type: 'circle', source: 'tasks', filter: ['!', ['has', 'point_count']],
        paint: {
            'circle-color': '#1e1e1e', 'circle-radius': 14,
            'circle-stroke-width': 4, 'circle-stroke-color': ['get', 'color'],
            'circle-opacity': ['get', 'opacity'], 'circle-stroke-opacity': ['get', 'opacity'],
        },
    });
    map.addLayer({
        id: 'pt-emoji', type: 'symbol', source: 'tasks', filter: ['!', ['has', 'point_count']],
        layout: { 'icon-image': ['get', 'icon'], 'icon-size': 0.5, 'icon-allow-overlap': true },
        paint: { 'icon-opacity': ['get', 'opacity'] },
    });

    // Per-trip START (green pennant, banner LEFT) / FINISH (chequered, banner
    // RIGHT) flags. icon-anchor 'bottom' + the FLAG_*_OFF_X offset land the POLE
    // BASE on the trip endpoint (see the FLAG_* constants); the alpha channel is
    // the click mask (flagHitAlpha). ABOVE the client circles, BELOW cluster-count.
    map.addLayer({
        id: 'trip-marks', type: 'symbol', source: 'trip-marks',
        layout: {
            'icon-image': ['case', ['==', ['get', 'kind'], 'finish'], 'flag-finish', 'flag-start'],
            'icon-size': FLAG_ICON_SIZE, 'icon-allow-overlap': true, 'icon-anchor': 'bottom',
            'icon-offset': ['case', ['==', ['get', 'kind'], 'finish'],
                ['literal', [FLAG_FINISH_OFF_X, 0]], ['literal', [FLAG_START_OFF_X, 0]]],
            visibility: showTrips ? 'visible' : 'none',
        },
    });

    // Cluster count — ABOVE the flags so a flag landing on a cluster can't hide
    // its number.
    map.addLayer({
        id: 'cluster-count', type: 'symbol', source: 'tasks', filter: ['has', 'point_count'],
        layout: { 'text-field': ['get', 'point_count_abbreviated'], 'text-font': ['Noto Sans Bold'], 'text-size': 14 },
        paint: { 'text-color': '#ffffff' },
    });

    // In-progress cars — static last-known position of a running trip (same green
    // car glyph) until a live TRIP_LIVE takes over. Topmost, with the plate label.
    map.addLayer({
        id: 'progress', type: 'symbol', source: 'progress',
        layout: { 'icon-image': 'car-live', 'icon-size': 0.5, 'icon-allow-overlap': true,
            'text-field': ['get', 'plate'], 'text-font': ['Noto Sans Regular'], 'text-size': 10,
            'text-offset': [0, 1.4], 'text-anchor': 'top', visibility: showTrips ? 'visible' : 'none' },
        paint: { 'text-color': '#fff', 'text-halo-color': '#111', 'text-halo-width': 1 },
    });

    // Live cars (consent-gated TRIP_LIVE) — the very top layer.
    map.addLayer({
        id: 'live-cars', type: 'symbol', source: 'live',
        layout: { 'icon-image': 'car-live', 'icon-size': 0.5, 'icon-allow-overlap': true,
            'icon-rotate': ['get', 'heading'], 'icon-rotation-alignment': 'map',
            'text-field': ['get', 'plate'], 'text-font': ['Noto Sans Regular'], 'text-size': 10,
            'text-offset': [0, 1.4], 'text-anchor': 'top' },
        paint: { 'text-color': '#fff', 'text-halo-color': '#111', 'text-halo-width': 1 },
    });
}

function showPopup(lngLat, html) {
    if (!map) return;
    map._eckPopup?.remove();
    // className → dark styling (see :global(.eck-popup ...) below). maxWidth
    // 'none' lets the content size itself (the two-pane cluster popup is wider
    // than a single card). closeOnClick (default) closes it on an empty-map click.
    map._eckPopup = new maplibregl.Popup({ closeButton: true, maxWidth: 'none', className: 'eck-popup' })
        .setLngLat(lngLat).setHTML(html).addTo(map);
}

// Two-pane popup for a cluster: a scrollable LIST of its tasks (left) and, once
// a row is clicked, that task's full CARD (right) — both visible, so clicking
// another row just swaps the card (no Back needed; click the map to close both).
// On a narrow screen the panes wrap (card under list). Dark-styled (.eck-popup).
const CLUSTER_LIST_CAP = 100;
function clusterPanesHtml(leaves) {
    const shown = leaves.slice(0, CLUSTER_LIST_CAP);
    const rows = shown.map((l) => {
        const p = l.properties || {};
        const color = p.color || '#9ca3af';
        const name = String(p.name || '').trim();
        const ref = String(p.ref || p.fid || '—');
        const label = name ? `${name} <span style="color:#888">· ${ref}</span>` : ref;
        const dot = `<span style="display:inline-block;width:10px;height:10px;border-radius:50%;background:${color};margin-right:7px;flex:0 0 auto"></span>`;
        return `<div class="eck-row" onclick="window.__eckTaskCard &amp;&amp; window.__eckTaskCard('${p.fid}', this)" `
            + `style="display:flex;align-items:center;padding:6px 9px;cursor:pointer;font-size:0.85em;color:#ddd;`
            + `border-bottom:1px solid #2a2a2a;border-left:3px solid transparent">`
            + `${dot}<span style="overflow:hidden;text-overflow:ellipsis;white-space:nowrap">${label}</span></div>`;
    });
    const more = leaves.length > shown.length
        ? `<div style="color:#777;font-size:0.78em;padding:6px 9px">${tr('map.more', { count: leaves.length - shown.length })}</div>` : '';
    const listPane = `<div style="flex:1 1 175px;min-width:155px;border-right:1px solid #333">`
        + `<div style="font-weight:600;color:#fff;padding:9px 9px 5px;font-size:0.85em">${tr('map.tasks_here', { count: leaves.length })}</div>`
        + `<div class="eck-scroll" style="max-height:400px;overflow-y:auto">${rows.join('')}${more}</div></div>`;
    // Stable, initially-hidden card pane. Row clicks fill ONLY this (via the DOM,
    // not a full re-render) so the list keeps its scroll position.
    const cardPane = `<div id="eck-card" class="eck-scroll" `
        + `style="display:none;flex:1 1 250px;min-width:225px;max-height:420px;overflow-y:auto;padding:9px 11px"></div>`;
    return `<div style="display:flex;flex-wrap:wrap;align-items:stretch;font-family:var(--font-family);max-width:min(560px,90vw)">`
        + `${listPane}${cardPane}</div>`;
}

const __summaryCache = {};
function escHtml(s) {
    return String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}
// From an AI summary, take everything from "=== TECHNISCHE DETAILS ===" onward,
// with that header line itself dropped.
function extractTechDetails(sum) {
    if (!sum) return '';
    const marker = '=== TECHNISCHE DETAILS ===';
    const idx = sum.indexOf(marker);
    if (idx < 0) return '';
    return sum.slice(idx + marker.length).replace(/^\s*\n/, '').trim();
}
// Show a single task's card (lone marker click) — same #eck-card container +
// AI summary as the cluster card, so a single ticket looks identical.
function showTaskCard(lngLat, props) {
    const fid = props.fid;
    const html = `<div id="eck-card" class="eck-scroll" data-fid="${fid}" `
        + `style="max-width:min(340px,90vw);max-height:440px;overflow-y:auto;padding:9px 11px">`
        + `${props.popup}<div id="eck-sum"></div></div>`;
    showPopup(lngLat, html);
    loadCardSummary(fid);
}

// Fetch a ticket's AI summary on demand and inject the TECHNISCHE-DETAILS
// excerpt at the bottom of its card — only if that card is still on screen.
async function loadCardSummary(fid) {
    const sep = fid.indexOf(':');
    const table = fid.slice(0, sep), id = fid.slice(sep + 1);
    if (table !== 'document') return; // only support tickets carry this summary
    if (__summaryCache[fid] === undefined) {
        try {
            const res = await api.get(`/api/support/tickets/${id}/threads?meta_only=true`);
            __summaryCache[fid] = extractTechDetails(res && res.ai_summary);
        } catch (e) { __summaryCache[fid] = ''; }
    }
    const excerpt = __summaryCache[fid];
    if (!excerpt) return;
    const card = document.getElementById('eck-card');
    const slot = document.getElementById('eck-sum');
    if (!card || !slot || card.getAttribute('data-fid') !== fid) return; // moved on
    slot.innerHTML = `<div style="margin-top:9px;border-top:1px solid #333;padding-top:7px">`
        + `<div style="white-space:pre-wrap;color:#b3b3b3;font-size:0.78em;line-height:1.4;max-height:240px;overflow:hidden">`
        + `${escHtml(excerpt)}</div></div>`;
}

// Per-pixel alpha test for a flag click. The flag icon isn't rotated, so
// screen→canvas is a pure scale: a 48-px canvas renders as (48/pixelRatio)·icon-
// size = 24·S display px, hence 1 screen px = pixelRatio/S = 2/S canvas px. The
// pole base (anchorX, size) is pinned to the projected trip endpoint, so we map
// the click's offset from there into canvas pixels and sample the icon's alpha.
// ≥ FLAG_ALPHA_HIT ⇒ a solid pixel (hit); anything else ⇒ a transparent hole and
// the caller falls through to whatever's beneath the flag.
const FLAG_ALPHA_HIT = 32;
function flagHitAlpha(feature, point) {
    const meta = iconPixels[feature.properties.kind === 'finish' ? 'flag-finish' : 'flag-start'];
    if (!meta || !map) return 0;
    const anchor = map.project(feature.geometry.coordinates);
    const k = 2 / FLAG_ICON_SIZE;   // canvas px per screen px (pixelRatio 2 ÷ icon-size)
    const cx = Math.round(meta.anchorX + (point.x - anchor.x) * k);
    let cy = Math.round(meta.anchorY + (point.y - anchor.y) * k);
    if (cy === meta.size) cy = meta.size - 1;   // bottom-edge anchor → sample the base row
    if (cx < 0 || cy < 0 || cx >= meta.size || cy >= meta.size) return 0;
    return meta.data[(cy * meta.size + cx) * 4 + 3];
}

function wireInteractions() {
    const pointer = (on) => () => { map.getCanvas().style.cursor = on ? 'pointer' : ''; };
    for (const layer of ['pt-ring', 'pt-emoji', 'clusters', 'ships-line', 'trips-line', 'trip-marks', 'progress', 'live-cars']) {
        map.on('mouseenter', layer, pointer(true));
        map.on('mouseleave', layer, pointer(false));
    }
    // ONE click handler with explicit priority. A cluster/marker circle is OPAQUE
    // to clicks (never falls through to a line beneath it); a flag is a per-pixel
    // MASK — a click landing in one of its transparent holes falls through to the
    // cluster/ring underneath.
    map.on('click', (e) => {
        const order = ['live-cars', 'progress', 'trip-marks', 'clusters', 'pt-ring', 'pt-emoji', 'ships-line', 'trips-line'];
        const layers = order.filter((l) => map.getLayer(l));
        const hits = map.queryRenderedFeatures(e.point, { layers });
        if (!hits.length) return;
        const pick = (id) => hits.find((h) => h.layer.id === id);

        // 1) A car (live or in-progress) is topmost & opaque → its trip popup.
        const car = pick('live-cars') || pick('progress');
        if (car) { if (car.properties?.popup) showPopup(e.lngLat, car.properties.popup); return; }

        // 2) A flag → per-pixel alpha test. A solid pixel shows the trip popup; a
        //    transparent hole falls through to the cluster/ring beneath. At a
        //    splayed leg junction take the first flag that's solid under the pointer.
        for (const f of hits) {
            if (f.layer.id !== 'trip-marks') continue;
            if (flagHitAlpha(f, e.point) >= FLAG_ALPHA_HIT) { showPopup(e.lngLat, f.properties.popup); return; }
        }

        // 3) Cluster → list its tasks. NOTE: the source's getClusterLeaves()
        //    callback never fires in this setup (silent worker no-reply), so we
        //    reconstruct the members ourselves: the shown features whose projected
        //    position falls within the cluster radius of its centre. That's the
        //    same partition supercluster uses, so neighbouring clusters
        //    (>clusterRadius apart) don't bleed in.
        const cluster = pick('clusters');
        if (cluster) {
            const center = map.project(cluster.geometry.coordinates);
            const R = 55; // ~clusterRadius (50) + a little slack
            const leaves = filteredTaskFeatures().filter((f) => {
                const p = map.project(f.geometry.coordinates);
                return Math.hypot(p.x - center.x, p.y - center.y) <= R;
            });
            if (leaves.length) {
                lastClusterLeaves = leaves;
                lastClusterLngLat = cluster.geometry.coordinates;
                showPopup(lastClusterLngLat, clusterPanesHtml(leaves));
            }
            return;
        }
        // 4) Single task marker → its full card (same card + AI summary as the
        //    cluster's, just without the list beside it).
        const marker = pick('pt-ring') || pick('pt-emoji');
        if (marker?.properties?.popup) { showTaskCard(e.lngLat, marker.properties); return; }
        // 5) Otherwise a shipment/trip line beneath → its own popup.
        const line = pick('ships-line') || pick('trips-line');
        if (line?.properties?.popup) { showPopup(e.lngLat, line.properties.popup); }
    });
}

async function initMap() {
    maplibregl = (await import('maplibre-gl')).default;
    map = new maplibregl.Map({
        container: mapContainer,
        style: styleUrl(currentMapStyle),
        center: [HOME_LOCATION.lng, HOME_LOCATION.lat],
        zoom: 6,
        // The OpenFreeMap style already carries proper attribution; don't add
        // our own on top (it just duplicated OSM/OpenFreeMap).
        attributionControl: true,
    });
    map.addControl(new maplibregl.NavigationControl({ showCompass: false }), 'top-right');

    await new Promise((res) => map.on('load', res));
    addImagesAndLayers();   // sources start empty; layers ready immediately
    wireInteractions();
    mapLoading = false;     // show the base map NOW — geocoding must not block it

    // Remember the last map view across reloads (persist on every settle).
    map.on('moveend', () => {
        try {
            const c = map.getCenter();
            localStorage.setItem(MAP_VIEW_KEY, JSON.stringify({ lng: c.lng, lat: c.lat, zoom: map.getZoom() }));
        } catch { /* private mode / quota */ }
    });
    // Initial framing: remembered view → else last trip (if < 24h) → else HQ.
    applyInitialView();

    // Fill markers + routes off the critical render path. Geocoding is rate-
    // limited (Nominatim 1 req/s), so it can't gate the map appearing. Points
    // with stored coords show at once; address-only ones trickle in. geoCache
    // (localStorage) makes subsequent loads instant. The view is set by
    // applyInitialView() and NOT re-fit here, so a remembered position sticks.
    (async () => {
        await buildTaskFeatures();
        refreshTaskSource();
        await buildShipmentFeatures();
        if (map && map.getSource('ships')) map.getSource('ships').setData(fc(shipmentFeatures));
    })();
}

// Choose the view on entry:
//   1. a remembered view (last position/zoom) if we have one;
//   2. else zoom to the last trip, if it started within the last 24h;
//   3. else just centre on our location (HQ).
async function applyInitialView() {
    if (!map || !maplibregl) return;
    try {
        const saved = JSON.parse(localStorage.getItem(MAP_VIEW_KEY) || 'null');
        if (saved && Number.isFinite(saved.lng) && Number.isFinite(saved.lat)) {
            map.jumpTo({ center: [saved.lng, saved.lat], zoom: saved.zoom ?? 11 });
            return;
        }
    } catch { /* fall through */ }

    try {
        const trips = await api.get('/api/trips?limit=1');
        const t = Array.isArray(trips) ? trips[0] : null;
        const startedMs = t?.started_at ? new Date(t.started_at).getTime() : NaN;
        if (Number.isFinite(startedMs) && (Date.now() - startedMs) < DAY_MS) {
            const full = await api.get(`/api/trips/${t.id}`).catch(() => null);
            const pts = (full?.points || []).filter(p => typeof p.lat === 'number' && typeof p.lng === 'number');
            if (pts.length && map) {
                const b = new maplibregl.LngLatBounds();
                for (const p of pts) b.extend([p.lng, p.lat]);
                try { map.fitBounds(b, { padding: 60, maxZoom: 14, duration: 0 }); return; } catch { /* single pt */ }
            }
        }
    } catch { /* no trips / offline → HQ */ }

    map.jumpTo({ center: [HOME_LOCATION.lng, HOME_LOCATION.lat], zoom: 11 });
}
</script>

<!-- Full-bleed map: fills the whole right pane; controls float on top. -->
<div class="map-fullbleed">
    {#if mapLoading}
        <div class="map-overlay">{$t('map.loading_map')}</div>
    {/if}
    <div bind:this={mapContainer} class="gl-map" class:hidden={loading}></div>

    <div class="map-controls">
        {#if assigneeChips.length > 0}
            <div class="assignee-chips" role="group" aria-label={$t('map.filter_assignee')}>
                {#each assigneeChips as chip (chip.key)}
                    <button type="button" class="chip" class:active={selectedAssignees.has(chip.key)}
                        on:click={() => toggleAssignee(chip.key)} title={`${chip.label} (${chip.count})`}>
                        {chip.label} <span class="chip-count">{chip.count}</span>
                    </button>
                {/each}
            </div>
        {/if}
        <!-- Search (replaces the removed dark/Fahrten/Live controls): type OR
             dictate; filters the task markers by number/customer. -->
        <div class="map-search">
            <input
                type="text"
                class="search-input"
                placeholder={$t('map.search_placeholder')}
                bind:value={searchQuery}
                on:input={runSearch}
                on:keydown={(e) => e.key === 'Enter' && runSearch()} />
            {#if searchQuery}
                <button type="button" class="search-btn" on:click={clearSearch} title={$t('map.clear_tip')}>✕</button>
            {/if}
            <button type="button" class="search-btn mic" class:active={listening} on:click={dictateSearch} title={$t('map.dictate_tip')}>🎤</button>
        </div>
    </div>
</div>

<style>
    /* Dark map popups (cluster list + task card), matching the left sidebar.
       :global because MapLibre renders popups outside this component's scope.
       The inner scroll panes inherit the app-wide dark scrollbar (app.css). */
    :global(.eck-popup .maplibregl-popup-content) {
        background: #1e1e1e;
        color: #e0e0e0;
        border: 1px solid #333;
        border-radius: 8px;
        padding: 0;
        overflow: hidden;
        box-shadow: 0 6px 20px rgba(0, 0, 0, 0.55);
    }
    :global(.eck-popup .maplibregl-popup-tip) { display: none; }
    :global(.eck-popup .maplibregl-popup-close-button) {
        color: #999;
        font-size: 18px;
        padding: 2px 7px;
        z-index: 2;
    }
    :global(.eck-popup .maplibregl-popup-close-button:hover) {
        background: transparent;
        color: #fff;
    }

    /* Full-bleed: the parent `.content.fullbleed` drops its padding and clips,
       so the map just fills it (no negative-offset overshoot that used to spill
       past the pane and force a scrollbar). Controls float on top. */
    .map-fullbleed {
        position: absolute;
        inset: 0;
    }
    .map-controls {
        position: absolute; top: 10px; left: 10px; z-index: 10;
        display: flex; align-items: center; gap: 0.5rem; flex-wrap: wrap;
        max-width: calc(100% - 80px); /* leave the top-right corner for zoom +/− */
        background: rgba(18, 18, 18, 0.72); border: 1px solid #333; border-radius: 10px;
        padding: 6px 8px; backdrop-filter: blur(4px);
    }
    .assignee-chips { display: flex; flex-wrap: wrap; gap: 0.3rem; max-width: 52vw; }
    .chip {
        background: #2a2a2a; color: #888; border: 1px solid #444;
        padding: 0.2rem 0.6rem; border-radius: 999px; font-size: 0.75rem; cursor: pointer;
        transition: background 0.15s, color 0.15s, border-color 0.15s;
    }
    .chip:hover { border-color: #4a69bd; color: #ccc; }
    .chip.active { background: #1e3a5f; color: #93c5fd; border-color: #3b82f6; }
    .chip-count {
        display: inline-block; margin-left: 0.3rem; padding: 0 0.35rem;
        background: rgba(255,255,255,0.08); border-radius: 999px; font-size: 0.7rem;
    }
    .map-search { display: flex; align-items: center; gap: 4px; }
    .search-input {
        background: #2a2a2a; color: #eee; border: 1px solid #444;
        padding: 0.3rem 0.55rem; border-radius: 6px; font-size: 0.8rem; outline: none; width: 11rem;
    }
    .search-input::placeholder { color: #777; }
    .search-input:focus { border-color: #4a69bd; }
    .search-btn {
        background: #2a2a2a; color: #ccc; border: 1px solid #444;
        padding: 0.3rem 0.5rem; border-radius: 6px; font-size: 0.85rem; cursor: pointer; line-height: 1;
    }
    .search-btn:hover { border-color: #4a69bd; }
    .search-btn.mic.active { background: #7f1d1d; color: #fff; border-color: #ef4444; }
    .gl-map { position: absolute; inset: 0; }
    .gl-map.hidden { visibility: hidden; }
    .map-overlay {
        position: absolute; inset: 0; display: flex; align-items: center; justify-content: center;
        background: #1e1e1e; color: #888; z-index: 100; font-size: 1.2rem;
    }
    /* MapLibre popups render light; nudge them to match the dark dashboard. */
    :global(.maplibregl-popup-content) { border-radius: 6px; }

    /* Attribution is legally required (OSM/OpenFreeMap) but kept faint on the
       dark theme so it doesn't compete with the map. */
    :global(.maplibregl-ctrl-attrib) {
        background: rgba(20, 20, 20, 0.45) !important;
        font-size: 9px;
        padding: 0 5px;
    }
    :global(.maplibregl-ctrl-attrib),
    :global(.maplibregl-ctrl-attrib a) {
        color: rgba(200, 205, 210, 0.4) !important;
        text-decoration: none;
    }
    :global(.maplibregl-ctrl-attrib:hover),
    :global(.maplibregl-ctrl-attrib:hover a) {
        color: rgba(200, 205, 210, 0.75) !important;
    }

    /* Zoom (+/−) control — default white squares clash with the dark theme;
       restyle dark with light icons. */
    :global(.maplibregl-ctrl-group) {
        background: rgba(30, 30, 30, 0.9) !important;
        border: 1px solid #444 !important;
        box-shadow: 0 1px 4px rgba(0, 0, 0, 0.45) !important;
    }
    :global(.maplibregl-ctrl-group button) {
        width: 28px; height: 28px; background-color: transparent !important;
    }
    :global(.maplibregl-ctrl-group button + button) { border-top: 1px solid #444 !important; }
    :global(.maplibregl-ctrl-group button:not(:disabled):hover) {
        background-color: rgba(255, 255, 255, 0.1) !important;
    }
    :global(.maplibregl-ctrl-group .maplibregl-ctrl-icon) { filter: invert(0.85); }
</style>
