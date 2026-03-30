<!-- [MODULE: GEO_ROUTING START] -->
<svelte:head>
    <link rel="stylesheet" href="https://unpkg.com/leaflet@1.9.4/dist/leaflet.css" />
</svelte:head>

<script>
    import { onMount, onDestroy } from 'svelte';
    import { api } from '$lib/api';

    export let targetLat = null;
    export let targetLng = null;
    export let targetTitle = 'Target';
    export let officeLat = 50.14;
    export let officeLng = 8.57;

    let mapContainer;
    let map;
    let tasks = [];
    let loading = true;
    let error = null;

    function badgeClass(badge) {
        if (badge === 'Bingo') return 'badge-bingo';
        if (badge === 'Normal') return 'badge-normal';
        return 'badge-far';
    }

    function markerColor(cost) {
        if (cost < 5) return '#22c55e';
        if (cost < 20) return '#eab308';
        return '#ef4444';
    }

    function emojiIcon(emoji, size = 28) {
        // Dynamically import L since leaflet needs browser context
        const L = window.L;
        return L.divIcon({
            html: `<span style="font-size:${size}px;line-height:1">${emoji}</span>`,
            className: 'emoji-marker',
            iconSize: [size, size],
            iconAnchor: [size / 2, size / 2],
            popupAnchor: [0, -size / 2],
        });
    }

    function taskIcon(cost) {
        const color = markerColor(cost);
        const L = window.L;
        return L.divIcon({
            html: `<div style="
                width:24px;height:24px;border-radius:50%;
                background:${color};border:2px solid #fff;
                display:flex;align-items:center;justify-content:center;
                font-size:13px;box-shadow:0 1px 4px rgba(0,0,0,0.4);
            ">&#x1f527;</div>`,
            className: 'emoji-marker',
            iconSize: [28, 28],
            iconAnchor: [14, 14],
            popupAnchor: [0, -14],
        });
    }

    onMount(async () => {
        const L = await import('leaflet');
        window.L = L.default || L;

        try {
            const res = await api.get(`/api/geo/nearby?target_lat=${targetLat}&target_lng=${targetLng}`);
            tasks = res;
        } catch (e) {
            error = e.message || 'Failed to load nearby tasks';
            loading = false;
            return;
        }

        loading = false;

        // Initialize map
        map = window.L.map(mapContainer).setView([targetLat, targetLng], 7);

        window.L.tileLayer('https://{s}.tile.openstreetmap.org/{z}/{x}/{y}.png', {
            attribution: '&copy; OpenStreetMap contributors',
            maxZoom: 18,
        }).addTo(map);

        // Office marker
        window.L.marker([officeLat, officeLng], { icon: emojiIcon('🏢', 30) })
            .addTo(map)
            .bindPopup('<b>Office (Eschborn)</b>');

        // Target marker
        window.L.marker([targetLat, targetLng], { icon: emojiIcon('⭐', 30) })
            .addTo(map)
            .bindPopup(`<b>${targetTitle}</b>`);

        // Route line
        window.L.polyline(
            [[officeLat, officeLng], [targetLat, targetLng]],
            { color: '#6366f1', weight: 3, dashArray: '8 6', opacity: 0.7 }
        ).addTo(map);

        // Task markers
        for (const t of tasks) {
            window.L.marker([t.lat, t.lng], { icon: taskIcon(t.cost) })
                .addTo(map)
                .bindPopup(`
                    <b>${t.orderNumber}</b><br/>
                    ${t.customerName}<br/>
                    <span style="color:${markerColor(t.cost)}">Cost: ${t.cost.toFixed(2)}</span> &middot; ${t.distanceKm.toFixed(1)} km
                `);
        }

        // Fit bounds to show all markers
        const allPoints = [
            [officeLat, officeLng],
            [targetLat, targetLng],
            ...tasks.map(t => [t.lat, t.lng]),
        ];
        if (allPoints.length > 1) {
            map.fitBounds(window.L.latLngBounds(allPoints), { padding: [40, 40] });
        }
    });

    onDestroy(() => {
        if (map) {
            map.remove();
            map = null;
        }
    });
</script>

<div class="geo-route-map-container">
    <div class="map-panel">
        {#if loading}
            <div class="map-loading">Loading map...</div>
        {/if}
        {#if error}
            <div class="map-error">{error}</div>
        {/if}
        <div bind:this={mapContainer} class="leaflet-map" class:hidden={loading || error}></div>
    </div>

    <div class="list-panel">
        <h3>Nearby Tasks ({tasks.length})</h3>
        {#if tasks.length === 0 && !loading}
            <p class="empty">No geocoded tasks found.</p>
        {/if}
        <ul class="task-list">
            {#each tasks as task, i}
                <li class="task-item">
                    <div class="task-rank">#{i + 1}</div>
                    <div class="task-info">
                        <div class="task-title">{task.orderNumber}</div>
                        <div class="task-sub">{task.customerName}</div>
                        <div class="task-meta">
                            {task.distanceKm.toFixed(1)} km &middot; cost {task.cost.toFixed(2)}
                        </div>
                    </div>
                    <span class="badge {badgeClass(task.badge)}">{task.badge}</span>
                </li>
            {/each}
        </ul>
    </div>
</div>

<style>
    .geo-route-map-container {
        display: flex;
        gap: 1rem;
        height: 480px;
    }

    .map-panel {
        flex: 2;
        position: relative;
        border-radius: 8px;
        overflow: hidden;
        border: 1px solid #333;
    }

    .leaflet-map { width: 100%; height: 100%; }
    .leaflet-map.hidden { visibility: hidden; }

    .map-loading, .map-error {
        position: absolute;
        inset: 0;
        display: flex;
        align-items: center;
        justify-content: center;
        background: #1e1e1e;
        color: #888;
        z-index: 1000;
    }
    .map-error { color: #ef4444; }

    .list-panel {
        flex: 1;
        min-width: 240px;
        max-height: 480px;
        overflow-y: auto;
        background: #1e1e1e;
        border: 1px solid #333;
        border-radius: 8px;
        padding: 1rem;
    }

    .list-panel h3 {
        color: #ccc;
        font-size: 1rem;
        margin: 0 0 0.75rem 0;
        border-bottom: 1px solid #333;
        padding-bottom: 0.5rem;
    }

    .empty { color: #666; font-style: italic; font-size: 0.9rem; }

    .task-list { list-style: none; padding: 0; margin: 0; }

    .task-item {
        display: flex;
        align-items: center;
        gap: 0.75rem;
        padding: 0.6rem 0;
        border-bottom: 1px solid #2a2a2a;
    }
    .task-item:last-child { border-bottom: none; }

    .task-rank {
        color: #555;
        font-size: 0.8rem;
        font-weight: 700;
        min-width: 24px;
    }

    .task-info { flex: 1; min-width: 0; }
    .task-title { color: #ddd; font-weight: 600; font-size: 0.9rem; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
    .task-sub { color: #888; font-size: 0.8rem; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
    .task-meta { color: #666; font-size: 0.75rem; font-family: monospace; margin-top: 2px; }

    .badge {
        padding: 0.15rem 0.5rem;
        border-radius: 9999px;
        font-size: 0.7rem;
        font-weight: 600;
        white-space: nowrap;
    }
    .badge-bingo { background: #14532d; color: #4ade80; }
    .badge-normal { background: #422006; color: #facc15; }
    .badge-far { background: #450a0a; color: #f87171; }

    :global(.emoji-marker) {
        background: none !important;
        border: none !important;
    }

    @media (max-width: 700px) {
        .geo-route-map-container { flex-direction: column; height: auto; }
        .map-panel { height: 300px; }
        .list-panel { max-height: 300px; }
    }
</style>
<!-- [MODULE: GEO_ROUTING END] -->
