<script>
    import { page } from '$app/stores';
    import { onMount } from 'svelte';
    import { api } from '$lib/api';
    import { goto } from '$app/navigation';
    import { base } from '$app/paths';
    import { toastStore } from '$lib/stores/toastStore';

    let rmaId = $page.params.id;
    let isNew = rmaId === 'new';
    let loading = !isNew;

    // Form Data
    let formData = {
        orderType: 'rma',
        rmaNumber: '',
        customerName: '',
        customerEmail: '',
        productSku: '',
        productName: '',
        issueDescription: '',
        status: 'pending',
        priority: 'normal',
        metadata: {},
    };

    onMount(async () => {
        if (!isNew) {
            await loadRMA();
        } else {
            formData.rmaNumber = 'AUTO-GEN';
            // Pre-fill from URL params when coming from a Support ticket
            const params = $page.url.searchParams;
            const linkedTicketId = params.get('ticketId');
            if (linkedTicketId) {
                formData.metadata = { ticketId: linkedTicketId };
                formData.customerName    = params.get('name')  || '';
                formData.customerEmail   = params.get('email') || '';
                formData.issueDescription = params.get('issue') || '';
            }

            const linkedSerial = params.get('serial');
            const linkedModel = params.get('model');
            if (linkedSerial) formData.serialNumber = linkedSerial;
            if (linkedModel) formData.productSku = linkedModel;
        }
    });

    async function loadRMA() {
        try {
            const data = await api.get(`/rma/${rmaId}`);
            formData = { ...data };
        } catch (e) {
            toastStore.add('Error loading RMA', 'error');
            goto(`${base}/dashboard/rma`);
        } finally {
            loading = false;
        }
    }

    async function handleSubmit() {
        try {
            if (isNew) {
                // Auto-gen handled by backend if empty, or we pass specific format
                // Simple validation
                if (!formData.customerName || !formData.productSku) {
                    toastStore.add('Customer Name and Product SKU are required', 'warning');
                    return;
                }

                // If rmaNumber is placeholder, clear it so backend generates it
                if(formData.rmaNumber === 'AUTO-GEN') delete formData.rmaNumber;

                await api.post('/rma', formData);
                toastStore.add('RMA Created Successfully', 'success');
            } else {
                await api.put(`/rma/${rmaId}`, formData);
                toastStore.add('RMA Updated', 'success');
            }
            goto(`${base}/dashboard/rma`);
        } catch (e) {
            toastStore.add(`Error: ${e.message}`, 'error');
        }
    }

    async function deleteRMA() {
        if(!confirm('Are you sure you want to delete this RMA?')) return;

        try {
            await api.delete(`/rma/${rmaId}`);
            toastStore.add('RMA Deleted', 'success');
            goto(`${base}/dashboard/rma`);
        } catch (e) {
            toastStore.add(e.message, 'error');
        }
    }

    function goBack() {
        goto(`${base}/dashboard/rma`);
    }
</script>

<div class="detail-page">
    <div class="header">
        <button class="back-btn" on:click={goBack}>← Back</button>
        <div class="title-row">
            <h1>{isNew ? 'New RMA Request' : `RMA ${formData.rmaNumber}`}</h1>
            {#if !isNew}
                <button class="delete-btn" on:click={deleteRMA}>Delete</button>
            {/if}
        </div>
    </div>

    {#if loading}
        <div class="loading">Loading...</div>
    {:else}
        <form class="form-grid" on:submit|preventDefault={handleSubmit}>
            <!-- Linked Support Ticket banner -->
            {#if formData.metadata?.ticketId}
                <div class="section full linked-banner">
                    <div class="linked-row">
                        <span class="linked-label">🔗 Linked Support Ticket</span>
                        <a
                            class="linked-link"
                            href="{base}/dashboard/support/{formData.metadata.ticketId}"
                        >
                            #{formData.metadata.ticketId} → View Ticket
                        </a>
                    </div>
                </div>
            {/if}

            <!-- Customer Info -->
            <div class="section">
                <h2>Customer Information</h2>
                <div class="field">
                    <label>Customer Name *</label>
                    <input type="text" bind:value={formData.customerName} required />
                </div>
                <div class="field">
                    <label>Email</label>
                    <input type="email" bind:value={formData.customerEmail} />
                </div>
            </div>

            <!-- Product Info -->
            <div class="section">
                <h2>Product Details</h2>
                <div class="field">
                    <label>Product SKU *</label>
                    <input type="text" bind:value={formData.productSku} required class="code-input" />
                </div>
                <div class="field">
                    <label>Product Name</label>
                    <input type="text" bind:value={formData.productName} />
                </div>
            </div>

            <!-- Issue -->
            <div class="section full">
                <h2>Issue Description</h2>
                <textarea bind:value={formData.issueDescription} rows="4"></textarea>
            </div>

            <!-- Status (Only for edit) -->
            {#if !isNew}
                <div class="section">
                    <h2>Status</h2>
                    <div class="field">
                        <label>Current Status</label>
                        <select bind:value={formData.status}>
                            <option value="pending">Pending</option>
                            <option value="received">Received</option>
                            <option value="processing">Processing</option>
                            <option value="completed">Completed</option>
                            <option value="cancelled">Cancelled</option>
                        </select>
                    </div>
                    <div class="field">
                        <label>Priority</label>
                        <select bind:value={formData.priority}>
                            <option value="low">Low</option>
                            <option value="normal">Normal</option>
                            <option value="high">High</option>
                            <option value="urgent">Urgent</option>
                        </select>
                    </div>
                </div>
            {/if}

            <div class="actions full">
                <button type="button" class="cancel-btn" on:click={goBack}>Cancel</button>
                <button type="submit" class="save-btn">{isNew ? 'Create Request' : 'Save Changes'}</button>
            </div>
        </form>
    {/if}
</div>

<style>
    .detail-page { max-width: 800px; margin: 0 auto; }

    .header { margin-bottom: 2rem; }
    .back-btn { background: none; border: none; color: #888; cursor: pointer; font-size: 1rem; padding: 0; margin-bottom: 1rem; }
    .back-btn:hover { color: #fff; }

    .title-row { display: flex; justify-content: space-between; align-items: center; }
    h1 { color: #fff; font-size: 2rem; margin: 0; }
    h2 { color: #ccc; font-size: 1.1rem; margin-bottom: 1rem; border-bottom: 1px solid #333; padding-bottom: 0.5rem; }

    .form-grid {
        display: grid;
        grid-template-columns: 1fr 1fr;
        gap: 1.5rem;
    }

    .section {
        background: #1e1e1e;
        border: 1px solid #333;
        border-radius: 8px;
        padding: 1.5rem;
        display: flex;
        flex-direction: column;
        gap: 1rem;
    }

    .section.full { grid-column: 1 / -1; }

    .field { display: flex; flex-direction: column; gap: 0.5rem; }

    label { color: #888; font-size: 0.85rem; font-weight: 500; }

    input, select, textarea {
        background: #121212;
        border: 1px solid #444;
        border-radius: 4px;
        padding: 0.8rem;
        color: #fff;
        font-size: 1rem;
        font-family: inherit;
    }

    input:focus, select:focus, textarea:focus {
        border-color: #4a69bd;
        outline: none;
    }

    .code-input { font-family: monospace; }

    .actions {
        margin-top: 1rem;
        display: flex;
        justify-content: flex-end;
        gap: 1rem;
    }

    .save-btn {
        background: #28a745;
        color: white;
        border: none;
        padding: 0.8rem 2rem;
        border-radius: 4px;
        font-weight: 600;
        cursor: pointer;
    }

    .save-btn:hover { background: #218838; }

    .cancel-btn {
        background: #333;
        color: #ccc;
        border: none;
        padding: 0.8rem 1.5rem;
        border-radius: 4px;
        cursor: pointer;
    }

    .cancel-btn:hover { background: #444; }

    .delete-btn {
        background: #d32f2f;
        color: white;
        border: none;
        padding: 0.6rem 1.2rem;
        border-radius: 4px;
        cursor: pointer;
    }

    .linked-banner {
        background: #1a2a1a;
        border-color: #22c55e;
        padding: 0.9rem 1.25rem;
    }
    .linked-row {
        display: flex;
        align-items: center;
        gap: 1rem;
        flex-wrap: wrap;
    }
    .linked-label { color: #4ade80; font-weight: 600; font-size: 0.9rem; }
    .linked-link {
        color: #93c5fd;
        text-decoration: none;
        font-family: monospace;
        font-size: 0.85rem;
        border-bottom: 1px dashed #4a69bd;
    }
    .linked-link:hover { color: #bfdbfe; border-bottom-color: #93c5fd; }

    @media (max-width: 700px) {
        .form-grid { grid-template-columns: 1fr; }
    }
</style>
