<script>
    import { onMount } from 'svelte';
    import { api } from '$lib/api';
    import { goto } from '$app/navigation';
    import { base } from '$app/paths';
    import { toastStore } from '$lib/stores/toastStore';

    let warehouses = [];
    let loading = true;
    let error = null;

    onMount(async () => {
        await loadWarehouses();
    });

    async function loadWarehouses() {
        try {
            warehouses = await api.get('/api/warehouse');
        } catch (e) {
            error = e.message;
            toastStore.add('Failed to load warehouses', 'error');
        } finally {
            loading = false;
        }
    }

    async function createWarehouse() {
        const name = prompt("Enter warehouse name:");
        if (!name) return;

        try {
            const newWh = await api.post('/api/warehouse', { name });
            toastStore.add('Warehouse created', 'success');
            warehouses = [...warehouses, newWh];
        } catch (e) {
            toastStore.add(e.message, 'error');
        }
    }

    function openWarehouse(id) {
        goto(`${base}/dashboard/warehouse/${id}`);
    }
</script>

<div class="warehouse-page">
    <header>
        <h1>Warehouses</h1>
        <div class="actions">
            <a href="{base}/dashboard/warehouse/blueprint" class="action-btn secondary">Blueprint Editor</a>
            <button class="action-btn primary" on:click={createWarehouse}>+ New Warehouse</button>
        </div>
    </header>

    {#if loading}
        <div class="loading">Loading...</div>
    {:else if error}
        <div class="error">{error}</div>
    {:else}
        <div class="grid-container">
            {#if warehouses.length === 0}
                <div class="empty-state">No warehouses found. Create one to start.</div>
            {/if}

            {#each warehouses as wh}
                <!-- svelte-ignore a11y-click-events-have-key-events -->
                <!-- svelte-ignore a11y-no-static-element-interactions -->
                <div class="card wh-card" on:click={() => openWarehouse(wh.id)}>
                    <div class="card-body">
                        <h3>{wh.name}</h3>
                        <p class="desc">{wh.description || 'No description'}</p>
                    </div>
                    <div class="card-footer">
                        <div class="stat">
                            <span class="label">Racks</span>
                            <span class="value">{wh.racks ? wh.racks.length : 0}</span>
                        </div>
                        <div class="stat">
                            <span class="label">Status</span>
                            <span class="value status {wh.is_active ? 'active' : 'inactive'}">
                                {wh.is_active ? 'Active' : 'Inactive'}
                            </span>
                        </div>
                    </div>
                </div>
            {/each}
        </div>
    {/if}
</div>

<style>
    header {
        display: flex;
        justify-content: space-between;
        align-items: center;
        margin-bottom: 2rem;
    }

    h1 { font-size: 1.8rem; color: #fff; margin: 0; }

    .action-btn {
        padding: 0.6rem 1.2rem;
        border-radius: 4px;
        border: none;
        font-weight: 600;
        cursor: pointer;
    }

    .action-btn.primary { background: #4a69bd; color: white; }
    .action-btn.secondary { background: #333; color: #ddd; text-decoration: none; }
    .action-btn.secondary:hover { background: #444; }

    .grid-container {
        display: grid;
        grid-template-columns: repeat(auto-fill, minmax(300px, 1fr));
        gap: 1.5rem;
    }

    .card {
        background: #1e1e1e;
        border: 1px solid #333;
        border-radius: 8px;
        padding: 1.5rem;
        display: flex;
        flex-direction: column;
        transition: transform 0.2s, border-color 0.2s;
        cursor: pointer;
    }

    .card:hover {
        transform: translateY(-2px);
        border-color: #555;
        background: #252525;
    }

    .card-body h3 { margin: 0 0 0.5rem 0; color: #e0e0e0; font-size: 1.3rem; }
    .desc { color: #888; font-size: 0.9rem; margin-bottom: 1rem; }

    .card-footer {
        margin-top: auto;
        padding-top: 1rem;
        border-top: 1px solid #333;
        display: flex;
        justify-content: space-between;
    }

    .stat { display: flex; flex-direction: column; }
    .stat .label { font-size: 0.7rem; color: #666; text-transform: uppercase; }
    .stat .value { font-size: 1rem; font-weight: 600; color: #fff; }

    .status.active { color: #28a745; }
    .status.inactive { color: #555; }

    .empty-state {
        grid-column: 1 / -1;
        text-align: center;
        padding: 3rem;
        color: #666;
        background: #1e1e1e;
        border-radius: 8px;
        border: 1px dashed #333;
    }
</style>
