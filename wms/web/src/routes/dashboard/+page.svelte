<script>
import { onMount, onDestroy } from 'svelte';
import { api } from '$lib/api';
import { base } from '$app/paths';
import { toastStore } from '$lib/stores/toastStore';
import { wsStore } from '$lib/stores/wsStore';
// Pure MapLibre GL JS (no Leaflet). Vector OpenFreeMap base + native
// data-layers for clustered task markers, shipment routes, trips and live cars.
import 'maplibre-gl/dist/maplibre-gl.css';

let loading = true;
let mapLoading = true;
let mapContainer;
let map;
let maplibregl = null;
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
let parkedFeatures = [];     // parked-car point features (trip end points)
const liveFeatures = new Map(); // trip_uuid → live-car point feature
const liveTimers = new Map();   // trip_uuid → expiry timeout
let showTrips = false;
const LIVE_STALE_MS = 90_000;

// Shipment SLA thresholds (days)
const SHIP_HIDE_DELIVERED_AFTER_DAYS = 3;
const SHIP_ALERT_UNDELIVERED_AFTER_DAYS = 7;

// Dashboard SLA scale — loaded from /api/admin/config/dashboard_sla on mount.
let agingScaleDays = 7;
let repairAgingScaleDays = 7;

// Marker urgency interpolation — blue (fresh) → green → yellow (full age).
// Red is reserved for escalated (manual/AI), never the age ramp.
const URGENCY_STOPS = [
    { t: 0.0,  rgb: [59, 130, 246]  }, // #3b82f6  blue
    { t: 0.45, rgb: [34, 197, 94]   }, // #22c55e  green
    { t: 1.0,  rgb: [251, 191, 36]  }, // #fbbf24  yellow
];
const ESCALATED_COLOR = '#ef4444';

function lerp(a, b, x) { return a + (b - a) * x; }
function urgencyColor(hours, scaleDays, escalated) {
    if (escalated) return ESCALATED_COLOR;
    const t = Math.max(0, Math.min(1, hours / Math.max(scaleDays * 24, 1)));
    let lo = URGENCY_STOPS[0], hi = URGENCY_STOPS[URGENCY_STOPS.length - 1];
    for (let i = 0; i < URGENCY_STOPS.length - 1; i++) {
        if (t >= URGENCY_STOPS[i].t && t <= URGENCY_STOPS[i + 1].t) {
            lo = URGENCY_STOPS[i]; hi = URGENCY_STOPS[i + 1]; break;
        }
    }
    const span = (hi.t - lo.t) || 1;
    const local = (t - lo.t) / span;
    const r = Math.round(lerp(lo.rgb[0], hi.rgb[0], local));
    const g = Math.round(lerp(lo.rgb[1], hi.rgb[1], local));
    const b = Math.round(lerp(lo.rgb[2], hi.rgb[2], local));
    return `#${[r, g, b].map(v => v.toString(16).padStart(2, '0')).join('')}`;
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
let geoCache = {};

onMount(async () => {
    try { geoCache = JSON.parse(localStorage.getItem(GEO_CACHE_KEY) || '{}'); } catch { geoCache = {}; }
    currentMapStyle = localStorage.getItem('eck_map_style') || 'dark';

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
                if (t.status.toLowerCase() !== 'closed') return true;
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
                toastStore.add('Pin moved to HQ', 'success');
            } catch (err) {
                console.error('reset-to-HQ failed', err);
                toastStore.add('Reset failed: ' + err.message, 'error');
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
                toastStore.add(`Besuch geplant: ${reference}`, 'success');
            } catch (err) {
                console.error('plan-visit failed', err);
                toastStore.add('Planen fehlgeschlagen: ' + err.message, 'error');
            }
        };

        window.__eckResolveShip = async function(id) {
            try {
                await api.post(`/api/delivery/shipments/${id}/resolve`, {});
                toastStore.add('Shipment marked as delivered', 'success');
                map._eckPopup?.remove();
                const fresh = await api.get('/api/delivery/shipments');
                activeShipments = (fresh || []).filter(s => s.status !== 'cancelled');
                await buildShipmentFeatures();
                if (map.getSource('ships')) map.getSource('ships').setData(fc(shipmentFeatures));
            } catch (e) {
                toastStore.add(`Resolve failed: ${e.message || e}`, 'error');
            }
        };

        loadTripsOverlay();
    } catch (e) {
        console.error('Failed to load dashboard data', e);
    } finally {
        loading = false;
    }
});

onDestroy(() => {
    for (const t of liveTimers.values()) clearTimeout(t);
    liveTimers.clear();
    liveFeatures.clear();
    if (map) { map.remove(); map = null; }
    if (typeof window !== 'undefined') {
        delete window.__eckResetToHQ;
        delete window.__eckPlanVisit;
        delete window.__eckResolveShip;
    }
});

function fc(features) { return { type: 'FeatureCollection', features }; }
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
    if (task._type === 'repair') return 'Repairs';
    return task.assignee_name || 'Unassigned';
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

// Tasks visible under the current assignee filter.
function filteredTaskFeatures() {
    return taskFeatures.filter(f => selectedAssignees.has(f.properties.assigneeKey));
}
function refreshTaskSource() {
    if (map && map.getSource('tasks')) map.getSource('tasks').setData(fc(filteredTaskFeatures()));
}

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
function taskUrgencyMeta(task) {
    const dateStr = task.date || new Date().toISOString();
    const hours = (Date.now() - new Date(dateStr).getTime()) / (1000 * 60 * 60);
    const scaleDays = task._type === 'repair' ? repairAgingScaleDays : agingScaleDays;
    const escalated = task.escalated === true || task.ai_priority === 'critical'
        || task.meta?.escalated === true || task.meta?.ai_priority === 'critical';
    const color = urgencyColor(hours, scaleDays, escalated);
    const score = escalated ? 2 : Math.max(0, Math.min(1, hours / Math.max(scaleDays * 24, 1)));
    return { hours, scaleDays, escalated, color, score };
}

// Status → emoji + opacity + grayscale fade (closed tasks). Color stays the
// urgency hue unless closed (grey).
function taskVisual(task) {
    const { hours, color: ageColor, escalated, score } = taskUrgencyMeta(task);
    let emoji = task._type === 'ticket' ? '💬' : '🔧';
    let color = ageColor;
    let opacity = task._type === 'ticket' ? 0.8 : 1.0;
    const status = (task.status || '').toLowerCase();
    const isClosed = status === 'completed' || status === 'cancelled' || status === 'closed';
    if (isClosed) {
        emoji = status === 'cancelled' ? '✖️' : '✅';
        color = '#6b7280';
        opacity = Math.max(0.1, opacity - (hours / 48));
    } else {
        if (status === 'waiting_parts' || status === 'received') emoji = '📦';
        else if (status === 'processing') emoji = '🔧';
        else if (status === 'on_hold' || status === 'onhold') emoji = '⏸️';
        else if (status === 'pending' || status === 'open') emoji = '⏳';
    }
    return { emoji, color, opacity, escalated, score: isClosed ? 0 : score };
}

function taskPopupHtml(task, lat, lng, cleanId, backendTable) {
    const linkUrl = task._type === 'repair'
        ? `${base}/dashboard/repairs/${cleanId}` : `${base}/dashboard/support/${cleanId}`;
    const btnText = task._type === 'repair' ? 'Open Repair' : 'Open Ticket';
    const typeLabel = task._type === 'repair' ? 'Repair (Physical)' : 'Ticket (Online)';
    const assigneeLine = task._type === 'ticket' && task.assignee_name
        ? `<div style="color:#888;font-size:0.8em;margin-top:2px">👤 ${task.assignee_name}</div>` : '';
    return `<div style="font-family:sans-serif;min-width:150px">
        <div style="font-size:0.7em;color:#a855f7;text-transform:uppercase;margin-bottom:2px;font-weight:bold;">${typeLabel}</div>
        <strong style="color:#333">${task.reference}</strong><br/>
        <span style="color:#666">${task.customer_name}</span><br/>
        <span style="color:#666;font-size:0.85em">Status: <b>${task.status}</b></span>
        ${assigneeLine}
        <div style="display:flex;gap:4px;margin-top:8px;flex-wrap:wrap">
            <a href="${linkUrl}" style="background:#4a69bd;color:#fff;padding:4px 8px;text-decoration:none;border-radius:4px;font-size:0.85em">${btnText}</a>
            <button type="button" onclick="window.__eckResetToHQ &amp;&amp; window.__eckResetToHQ('${backendTable}','${cleanId}')"
               style="background:#2a2a2a;color:#fbbf24;border:1px solid #f59e0b;padding:4px 8px;border-radius:4px;font-size:0.85em;cursor:pointer">📍 Reset to HQ</button>
            <button type="button" onclick="window.__eckPlanVisit &amp;&amp; window.__eckPlanVisit('${lat}','${lng}','${(task.reference || '').replace(/'/g, '')}','${backendTable}','${cleanId}')"
               style="background:#1e3a8a;color:#93c5fd;border:1px solid #3b82f6;padding:4px 8px;border-radius:4px;font-size:0.85em;cursor:pointer">📅 Besuch planen</button>
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

        out.push({
            type: 'Feature',
            geometry: { type: 'Point', coordinates: [lng, lat] },
            properties: {
                fid: `${backendTable}:${cleanId}`,
                assigneeKey: taskAssigneeKey(task),
                color: v.color,
                icon: `em-${v.emoji}`,
                opacity: v.opacity,
                score: v.score,
                popup: taskPopupHtml(task, lat, lng, cleanId, backendTable),
            },
        });
    }
    taskFeatures = out;
}

async function buildShipmentFeatures() {
    const out = [];
    const now = Date.now(), DAY_MS = 86_400_000;
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
                ? `<div style="font-family:sans-serif;min-width:180px">
                     <div style="color:#ef4444;font-weight:700;font-size:0.85em;margin-bottom:4px">⚠️ Stuck ${Math.floor(createdAgeDays)} days</div>
                     <strong style="color:#333">${ship.provider.toUpperCase()} · ${ship.tracking_number}</strong><br/>
                     <span style="color:#666;font-size:0.8em">Carrier status: <b>${ship.status}</b></span>
                     <div style="color:#888;font-size:0.75em;margin-top:4px">Possible carrier feed glitch — confirm delivery manually.</div>
                     <button type="button" onclick="window.__eckResolveShip &amp;&amp; window.__eckResolveShip('${shipId}')"
                        style="margin-top:8px;background:#ef4444;color:#fff;border:none;padding:5px 10px;border-radius:4px;font-size:0.8em;cursor:pointer">Mark as delivered</button>
                   </div>`
                : `<div style="font-family:sans-serif">📦 ${ship.provider.toUpperCase()} · ${ship.tracking_number}</div>`;
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
async function loadTripsOverlay() {
    try {
        const trips = await api.get('/api/trips?limit=20');
        if (!Array.isArray(trips) || !map) return;
        const lines = [], parked = [];
        for (const t of trips) {
            const full = await api.get(`/api/trips/${t.id}`).catch(() => null);
            const pts = (full?.points || [])
                .filter(p => typeof p.lat === 'number' && typeof p.lng === 'number')
                .sort((a, b) => (a.seq || 0) - (b.seq || 0))
                .map(p => [p.lng, p.lat]);
            if (pts.length < 2) continue;
            const km = full.computed_distance_km != null ? `${full.computed_distance_km} km (geschätzt)` : '';
            const sealed = full.seal_hash ? '🔒 GoBD-versiegelt' : '';
            const popup = `<div style="font-family:sans-serif">
                <strong>${full.vehicle_plate ? escPlate(full.vehicle_plate) + ' · ' : ''}Fahrt ${(full.started_at || '').slice(0, 16).replace('T', ' ')}</strong><br/>
                <span style="color:#666;font-size:0.85em">${km}</span><br/>
                <span style="color:#22c55e;font-size:0.8em">${sealed}</span></div>`;
            lines.push({ type: 'Feature', geometry: { type: 'LineString', coordinates: pts }, properties: { popup } });
            parked.push({ type: 'Feature', geometry: { type: 'Point', coordinates: pts[pts.length - 1] },
                properties: { plate: full.vehicle_plate || '', popup } });
        }
        tripFeatures = lines;
        parkedFeatures = parked;
        if (map.getSource('trips')) map.getSource('trips').setData(fc(tripFeatures));
        if (map.getSource('parked')) map.getSource('parked').setData(fc(parkedFeatures));
    } catch (e) {
        console.warn('trips overlay failed', e);
    }
}

function toggleTrips() {
    showTrips = !showTrips;
    const v = showTrips ? 'visible' : 'none';
    for (const id of ['trips-line', 'parked-cars', 'parked-plate']) {
        if (map && map.getLayer(id)) map.setLayoutProperty(id, 'visibility', v);
    }
}

// ── Live trips (consent-gated TRIP_LIVE) ───────────────────────────────────
function handleTripLive(msg) {
    if (!msg || msg.type !== 'TRIP_LIVE' || !map) return;
    if (Date.now() - (msg._receivedAt || 0) > 5000) return;
    const id = msg.trip_uuid;
    if (!id || typeof msg.lat !== 'number' || typeof msg.lng !== 'number') return;
    liveFeatures.set(id, {
        type: 'Feature',
        geometry: { type: 'Point', coordinates: [msg.lng, msg.lat] },
        properties: {
            plate: msg.vehicle_plate || '',
            heading: (typeof msg.heading === 'number' && !Number.isNaN(msg.heading)) ? msg.heading : 0,
        },
    });
    if (map.getSource('live')) map.getSource('live').setData(fc([...liveFeatures.values()]));
    if (liveTimers.has(id)) clearTimeout(liveTimers.get(id));
    liveTimers.set(id, setTimeout(() => {
        liveFeatures.delete(id);
        liveTimers.delete(id);
        if (map && map.getSource('live')) map.getSource('live').setData(fc([...liveFeatures.values()]));
    }, LIVE_STALE_MS));
}
$: if ($wsStore.lastMessage) handleTripLive($wsStore.lastMessage);

// ── Map setup ──────────────────────────────────────────────────────────────
function handleStyleChange() {
    localStorage.setItem('eck_map_style', currentMapStyle);
    if (!map) return;
    map.setStyle(styleUrl(currentMapStyle));
    map.once('style.load', addImagesAndLayers); // setStyle wipes custom sources/layers
}

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

function addImagesAndLayers() {
    if (!map) return;
    // Icon images (emoji + cars). Guard against re-adds after setStyle.
    for (const e of EMOJI_SET) {
        const id = `em-${e}`;
        if (!map.hasImage(id)) map.addImage(id, emojiImage(e, 48), { pixelRatio: 2 });
    }
    if (!map.hasImage('car-live')) map.addImage('car-live', carImage('#22c55e', '#0b3d1a'), { pixelRatio: 2 });
    if (!map.hasImage('car-parked')) map.addImage('car-parked', carImage('#9ca3af', '#374151'), { pixelRatio: 2 });

    const ringColor = ['case', ['>=', ['get', 'maxScore'], 2], ESCALATED_COLOR,
        ['interpolate', ['linear'], ['get', 'maxScore'], 0, '#3b82f6', 0.45, '#22c55e', 1, '#fbbf24']];

    // Tasks (clustered)
    if (!map.getSource('tasks')) {
        map.addSource('tasks', {
            type: 'geojson', data: fc(filteredTaskFeatures()),
            cluster: true, clusterRadius: 50, clusterMaxZoom: 14,
            clusterProperties: { maxScore: ['max', ['get', 'score']] },
        });
    } else {
        map.getSource('tasks').setData(fc(filteredTaskFeatures()));
    }
    map.addLayer({
        id: 'clusters', type: 'circle', source: 'tasks', filter: ['has', 'point_count'],
        paint: {
            'circle-color': '#1e1e1e',
            'circle-radius': ['step', ['get', 'point_count'], 16, 10, 20, 50, 26],
            'circle-stroke-width': 3, 'circle-stroke-color': ringColor,
        },
    });
    map.addLayer({
        id: 'cluster-count', type: 'symbol', source: 'tasks', filter: ['has', 'point_count'],
        layout: { 'text-field': ['get', 'point_count_abbreviated'], 'text-font': ['Noto Sans Bold'], 'text-size': 14 },
        paint: { 'text-color': '#ffffff' },
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

    // Home
    if (!map.getSource('home')) {
        map.addSource('home', { type: 'geojson', data: fc([{ type: 'Feature',
            geometry: { type: 'Point', coordinates: [HOME_LOCATION.lng, HOME_LOCATION.lat] }, properties: {} }]) });
    }
    map.addLayer({ id: 'home', type: 'symbol', source: 'home',
        layout: { 'icon-image': 'em-🏢', 'icon-size': 0.55, 'icon-allow-overlap': true } });

    // Shipment routes
    if (!map.getSource('ships')) map.addSource('ships', { type: 'geojson', data: fc(shipmentFeatures) });
    else map.getSource('ships').setData(fc(shipmentFeatures));
    map.addLayer({
        id: 'ships-line', type: 'line', source: 'ships',
        layout: { 'line-cap': 'round', 'line-join': 'round' },
        paint: { 'line-color': ['get', 'color'], 'line-width': ['get', 'width'],
            'line-opacity': ['get', 'opacity'], 'line-dasharray': [2, 2] },
    });

    // Historical trips
    if (!map.getSource('trips')) map.addSource('trips', { type: 'geojson', data: fc(tripFeatures) });
    else map.getSource('trips').setData(fc(tripFeatures));
    map.addLayer({
        id: 'trips-line', type: 'line', source: 'trips',
        layout: { 'line-cap': 'round', visibility: showTrips ? 'visible' : 'none' },
        paint: { 'line-color': '#a78bfa', 'line-width': 3, 'line-opacity': 0.75, 'line-dasharray': [2, 2] },
    });
    if (!map.getSource('parked')) map.addSource('parked', { type: 'geojson', data: fc(parkedFeatures) });
    else map.getSource('parked').setData(fc(parkedFeatures));
    map.addLayer({
        id: 'parked-cars', type: 'symbol', source: 'parked',
        layout: { 'icon-image': 'car-parked', 'icon-size': 0.5, 'icon-allow-overlap': true,
            visibility: showTrips ? 'visible' : 'none' },
    });
    map.addLayer({
        id: 'parked-plate', type: 'symbol', source: 'parked',
        layout: { 'text-field': ['get', 'plate'], 'text-font': ['Noto Sans Regular'], 'text-size': 10,
            'text-offset': [0, 1.4], 'text-anchor': 'top', visibility: showTrips ? 'visible' : 'none' },
        paint: { 'text-color': '#e5e7eb', 'text-halo-color': '#111', 'text-halo-width': 1 },
    });

    // Live cars
    if (!map.getSource('live')) map.addSource('live', { type: 'geojson', data: fc([...liveFeatures.values()]) });
    else map.getSource('live').setData(fc([...liveFeatures.values()]));
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
    map._eckPopup = new maplibregl.Popup({ closeButton: true, maxWidth: '280px' })
        .setLngLat(lngLat).setHTML(html).addTo(map);
}

function wireInteractions() {
    const pointer = (on) => () => { map.getCanvas().style.cursor = on ? 'pointer' : ''; };
    for (const layer of ['pt-ring', 'pt-emoji', 'clusters', 'ships-line', 'trips-line', 'parked-cars']) {
        map.on('mouseenter', layer, pointer(true));
        map.on('mouseleave', layer, pointer(false));
    }
    const popupOnClick = (layer) => map.on('click', layer, (e) => {
        const f = e.features?.[0];
        if (f?.properties?.popup) showPopup(e.lngLat, f.properties.popup);
    });
    popupOnClick('pt-ring');
    popupOnClick('pt-emoji');
    popupOnClick('ships-line');
    popupOnClick('trips-line');
    popupOnClick('parked-cars');

    // Cluster → expand zoom
    map.on('click', 'clusters', (e) => {
        const f = map.queryRenderedFeatures(e.point, { layers: ['clusters'] })[0];
        if (!f) return;
        map.getSource('tasks').getClusterExpansionZoom(f.properties.cluster_id, (err, zoom) => {
            if (err) return;
            map.easeTo({ center: f.geometry.coordinates, zoom });
        });
    });
}

function fitToData() {
    if (!map || !maplibregl) return;
    const b = new maplibregl.LngLatBounds([HOME_LOCATION.lng, HOME_LOCATION.lat], [HOME_LOCATION.lng, HOME_LOCATION.lat]);
    for (const f of taskFeatures) b.extend(f.geometry.coordinates);
    for (const f of shipmentFeatures) for (const c of f.geometry.coordinates) b.extend(c);
    try { map.fitBounds(b, { padding: 50, maxZoom: 12, duration: 0 }); } catch (e) { /* single point */ }
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

    // Fill markers + routes off the critical render path. Geocoding is rate-
    // limited (Nominatim 1 req/s), so it can't gate the map appearing. Points
    // with stored coords show at once; address-only ones trickle in. geoCache
    // (localStorage) makes subsequent loads instant.
    (async () => {
        await buildTaskFeatures();
        refreshTaskSource();
        fitToData();
        await buildShipmentFeatures();
        if (map && map.getSource('ships')) map.getSource('ships').setData(fc(shipmentFeatures));
        fitToData();
    })();
}
</script>

<!-- Full-bleed map: fills the whole right pane; controls float on top. -->
<div class="map-fullbleed">
    {#if mapLoading}
        <div class="map-overlay">Loading Map…</div>
    {/if}
    <div bind:this={mapContainer} class="gl-map" class:hidden={loading}></div>

    <div class="map-controls">
        {#if assigneeChips.length > 0}
            <div class="assignee-chips" role="group" aria-label="Filter by assignee">
                {#each assigneeChips as chip (chip.key)}
                    <button type="button" class="chip" class:active={selectedAssignees.has(chip.key)}
                        on:click={() => toggleAssignee(chip.key)} title={`${chip.label} (${chip.count})`}>
                        {chip.label} <span class="chip-count">{chip.count}</span>
                    </button>
                {/each}
            </div>
        {/if}
        <select bind:value={currentMapStyle} on:change={handleStyleChange} class="map-style-select">
            {#each MAP_PROVIDERS as provider}
                <option value={provider.id}>{provider.name}</option>
            {/each}
        </select>
        <button type="button" class="trips-toggle" class:active={showTrips} on:click={toggleTrips} title="Fahrten ein-/ausblenden">
            🚗 Fahrten
        </button>
        <span class="badge">Live</span>
    </div>
</div>

<style>
    /* Full-bleed: cancel the .content padding so the map fills the whole right
       pane (the .content is position:relative). Controls float on top. */
    .map-fullbleed {
        position: absolute;
        top: -2rem; left: -2rem; right: -2rem; bottom: -4rem;
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
    .map-style-select {
        background: #2a2a2a; color: #ccc; border: 1px solid #444;
        padding: 0.3rem 0.5rem; border-radius: 4px; font-size: 0.8rem; outline: none; cursor: pointer;
    }
    .map-style-select:focus { border-color: #4a69bd; }
    .badge {
        background: #1a3a1a; color: #4ade80; border: 1px solid #22c55e;
        padding: 0.2rem 0.5rem; border-radius: 4px; font-size: 0.7rem; font-weight: bold; text-transform: uppercase;
    }
    .gl-map { position: absolute; inset: 0; }
    .gl-map.hidden { visibility: hidden; }
    .map-overlay {
        position: absolute; inset: 0; display: flex; align-items: center; justify-content: center;
        background: #1e1e1e; color: #888; z-index: 100; font-size: 1.2rem;
    }
    .trips-toggle {
        background: #2a2a2a; color: #a78bfa; border: 1px solid #5b21b6;
        padding: 4px 10px; border-radius: 4px; font-size: 0.85em; cursor: pointer;
    }
    .trips-toggle.active { background: #5b21b6; color: #fff; }
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
