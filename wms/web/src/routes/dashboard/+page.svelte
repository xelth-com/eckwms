<script>
import { onMount } from 'svelte';
import { api } from '$lib/api';

let stats = {
    totalItems: 0,
    activeDevices: 0,
    pendingRMA: 0
};
let loading = true;

onMount(async () => {
    try {
        // In a real scenario, we'd have a /api/stats endpoint.
        // For now, we'll fetch items to count them as a placeholder.
        const items = await api.get('/api/items');
        stats.totalItems = items.length;

        // Devices endpoint isn't exposed in router.go yet for frontend, so we mock it or skip
    } catch (e) {
        console.error('Failed to load stats', e);
    } finally {
        loading = false;
    }
});
</script>

<div class="dashboard-home">
<header>
<h1>System Overview</h1>
</header>

<div class="stats-grid">
    <div class="stat-card primary">
        <div class="stat-value">{loading ? '...' : stats.totalItems}</div>
        <div class="stat-label">Total Items</div>
    </div>
    <div class="stat-card secondary">
        <div class="stat-value">--</div>
        <div class="stat-label">Active Scanners</div>
    </div>
    <div class="stat-card accent">
        <div class="stat-value">--</div>
        <div class="stat-label">Pending RMAs</div>
    </div>
</div>

<div class="activity-section">
    <h2>Recent Activity</h2>
    <div class="empty-state">No recent activity recorded.</div>
</div>
</div>

<style>
header { margin-bottom: 2rem; }
h1 { color: #fff; font-size: 1.8rem; margin: 0; }
h2 { color: #ccc; font-size: 1.2rem; margin-bottom: 1rem; border-bottom: 1px solid #333; padding-bottom: 0.5rem; }

.stats-grid {
display: grid;
grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
gap: 1.5rem;
margin-bottom: 3rem;
}

.stat-card {
background: #1e1e1e;
border: 1px solid #333;
border-radius: 8px;
padding: 1.5rem;
display: flex;
flex-direction: column;
align-items: flex-start;
}

.stat-card.primary { border-left: 4px solid #4a69bd; }
.stat-card.secondary { border-left: 4px solid #28a745; }
.stat-card.accent { border-left: 4px solid #f39c12; }

.stat-value { font-size: 2.5rem; font-weight: 700; color: #fff; line-height: 1; margin-bottom: 0.5rem; }
.stat-label { color: #888; font-size: 0.9rem; text-transform: uppercase; letter-spacing: 1px; }

.empty-state { color: #666; font-style: italic; padding: 2rem; text-align: center; background: #1e1e1e; border-radius: 8px; }
</style>
