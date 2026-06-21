<script>
    import { onMount } from 'svelte';
    import { api } from '$lib/api';
    import { goto } from '$app/navigation';
    import { base } from '$app/paths';
    import { toastStore } from '$lib/stores/toastStore';

    let rmas = [];
    let loading = true;
    let error = null;

    onMount(async () => {
        await loadRMAs();
    });

    async function loadRMAs() {
        try {
            // Fetch only RMA type orders
            rmas = await api.get('/api/rma?type=rma');
        } catch (e) {
            console.error(e);
            error = e.message;
            toastStore.add('Failed to load RMAs', 'error');
        } finally {
            loading = false;
        }
    }

    function openRMA(id) {
        goto(`${base}/dashboard/rma/${id}`);
    }

    function createNew() {
        goto(`${base}/dashboard/rma/new`);
    }

    function formatDate(dateStr) {
        if (!dateStr) return '-';
        return new Date(dateStr).toLocaleDateString();
    }
</script>

<div class="rma-page">
    <header>
        <h1>RMA Requests</h1>
        <div class="actions">
            <button class="action-btn primary" on:click={createNew}>+ New Request</button>
        </div>
    </header>

    {#if loading}
        <div class="loading">Loading requests...</div>
    {:else if error}
        <div class="error">{error}</div>
    {:else}
        <div class="table-container">
            {#if rmas.length === 0}
                <div class="empty-state">No RMA requests found.</div>
            {/if}

            <table class="rma-table">
                <thead>
                    <tr>
                        <th>RMA #</th>
                        <th>Customer</th>
                        <th>Product SKU</th>
                        <th>Date</th>
                        <th>Status</th>
                    </tr>
                </thead>
                <tbody>
                    {#each rmas as rma}
                        <!-- svelte-ignore a11y-click-events-have-key-events -->
                        <tr on:click={() => openRMA(rma.id)}>
                            <td class="code">{rma.rmaNumber}</td>
                            <td>{rma.customer_name}</td>
                            <td class="code-sm">{rma.product_sku}</td>
                            <td>{formatDate(rma.created_at)}</td>
                            <td>
                                <span class="status-badge {rma.status.toLowerCase()}">
                                    {rma.status}
                                </span>
                            </td>
                        </tr>
                    {/each}
                </tbody>
            </table>
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

    .table-container {
        background: #1e1e1e;
        border: 1px solid #333;
        border-radius: 8px;
        overflow-x: auto;
    }

    .rma-table {
        width: 100%;
        border-collapse: collapse;
        text-align: left;
    }

    .rma-table th {
        padding: 1rem;
        border-bottom: 1px solid #333;
        color: #888;
        font-size: 0.8rem;
        text-transform: uppercase;
        font-weight: 600;
    }

    .rma-table td {
        padding: 1rem;
        border-bottom: 1px solid #2a2a2a;
        color: #e0e0e0;
    }

    .rma-table tr {
        cursor: pointer;
        transition: background 0.1s;
    }

    .rma-table tr:hover {
        background: #252525;
    }

    .rma-table tr:last-child td {
        border-bottom: none;
    }

    .code { font-family: monospace; font-weight: bold; color: #4a69bd; }
    .code-sm { font-family: monospace; font-size: 0.9rem; color: #aaa; }

    .status-badge {
        padding: 2px 8px;
        border-radius: 12px;
        font-size: 0.75rem;
        font-weight: 600;
        text-transform: uppercase;
    }

    .status-badge.pending { background: #d35400; color: #fff; }
    .status-badge.received { background: #f39c12; color: #000; }
    .status-badge.processing { background: #3498db; color: #fff; }
    .status-badge.completed { background: #27ae60; color: #fff; }
    .status-badge.cancelled { background: #7f8c8d; color: #fff; }

    .empty-state { padding: 3rem; text-align: center; color: #666; }
</style>
