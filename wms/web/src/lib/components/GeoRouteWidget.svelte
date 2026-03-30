<!-- [MODULE: GEO_ROUTING START] -->
<script>
  import { onMount } from 'svelte';
  import { base } from '$app/paths';
  import api from '$lib/api';

  export let targetLat = null;
  export let targetLng = null;
  export let orderId = null;

  let tasks = [];
  let loading = false;
  let error = null;

  async function findNearby() {
    loading = true;
    error = null;
    tasks = [];

    try {
      // If no coords but we have an orderId, geocode first
      if ((targetLat == null || targetLng == null) && orderId) {
        const geo = await api.post(`/api/geo/geocode/${orderId}`);
        targetLat = geo.lat;
        targetLng = geo.lng;
      }

      if (targetLat == null || targetLng == null) {
        error = 'No coordinates available';
        loading = false;
        return;
      }

      tasks = await api.get(`/api/geo/nearby?target_lat=${targetLat}&target_lng=${targetLng}`);
    } catch (e) {
      error = e.message || 'Failed to fetch nearby tasks';
    } finally {
      loading = false;
    }
  }

  function badgeClass(badge) {
    if (badge === 'Bingo') return 'badge-bingo';
    if (badge === 'Normal') return 'badge-normal';
    return 'badge-far';
  }
</script>

<div class="geo-route-widget">
  <button on:click={findNearby} disabled={loading} class="find-btn">
    {#if loading}
      Searching...
    {:else}
      Find Nearby Tasks on Route
    {/if}
  </button>

  {#if error}
    <p class="error">{error}</p>
  {/if}

  {#if tasks.length > 0}
    <ul class="task-list">
      {#each tasks as task}
        <li class="task-item">
          <div class="task-header">
            <span class="task-title">{task.orderNumber} — {task.customerName}</span>
            <span class="badge {badgeClass(task.badge)}">{task.badge}</span>
          </div>
          <div class="task-meta">
            <span>{task.distanceKm.toFixed(1)} km to target</span>
            <span class="cost">cost: {task.cost.toFixed(2)}</span>
            <span class="status">{task.status}</span>
          </div>
        </li>
      {/each}
    </ul>
  {:else if !loading && !error}
    <p class="empty">Click the button to find tasks along the route.</p>
  {/if}
</div>

<style>
  .geo-route-widget {
    padding: 1rem;
  }

  .find-btn {
    padding: 0.6rem 1.2rem;
    background: #2563eb;
    color: #fff;
    border: none;
    border-radius: 6px;
    cursor: pointer;
    font-size: 0.95rem;
  }
  .find-btn:hover:not(:disabled) { background: #1d4ed8; }
  .find-btn:disabled { opacity: 0.6; cursor: not-allowed; }

  .error { color: #dc2626; margin-top: 0.5rem; }
  .empty { color: #6b7280; margin-top: 0.5rem; font-style: italic; }

  .task-list {
    list-style: none;
    padding: 0;
    margin-top: 1rem;
  }

  .task-item {
    padding: 0.75rem;
    border: 1px solid #e5e7eb;
    border-radius: 6px;
    margin-bottom: 0.5rem;
  }

  .task-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
  }

  .task-title { font-weight: 600; }

  .badge {
    padding: 0.15rem 0.5rem;
    border-radius: 9999px;
    font-size: 0.75rem;
    font-weight: 600;
  }
  .badge-bingo { background: #dcfce7; color: #166534; }
  .badge-normal { background: #fef9c3; color: #854d0e; }
  .badge-far { background: #fee2e2; color: #991b1b; }

  .task-meta {
    display: flex;
    gap: 1rem;
    margin-top: 0.4rem;
    font-size: 0.85rem;
    color: #6b7280;
  }

  .cost { font-family: monospace; }
</style>
<!-- [MODULE: GEO_ROUTING END] -->
