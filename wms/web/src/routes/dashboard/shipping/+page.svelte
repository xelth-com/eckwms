<script>
    import { onMount } from "svelte";
    import { api } from "$lib/api";
    import { toastStore } from "$lib/stores/toastStore.js";
    import { goto } from "$app/navigation";
    import { base } from "$app/paths";

    export let data;

    let pickings = data.pickings || [];
    let shipments = data.shipments || [];
    let providersConfig = data.providersConfig || { opal: false, dhl: false };
    let loading = false;
    let error = data.error || null;
    let activeTab = "pickings"; // 'pickings', 'shipments'
    let processingPickings = new Set();
    let isSyncingOpal = false;
    let isSyncingDhl = false;
    let expandedShipments = new Set();

    onMount(async () => {
        await loadData();
    });

    async function loadData() {
        loading = true;
        error = null;
        try {
            const [pickingsData, shipmentsData, configData] = await Promise.all([
                api.get("/api/odoo/pickings?state=assigned"),
                api.get("/api/delivery/shipments"),
                api.get("/api/delivery/config"),
            ]);
            pickings = pickingsData || [];
            shipments = shipmentsData || [];
            if (configData) providersConfig = configData;
        } catch (e) {
            console.error(e);
            error = e.message;
        } finally {
            loading = false;
        }
    }

    async function createShipment(pickingId) {
        if (processingPickings.has(pickingId)) return;
        processingPickings.add(pickingId);
        processingPickings = processingPickings;
        try {
            await api.post("/api/delivery/shipments", { pickingId, providerCode: "opal" });
            await loadData();
            activeTab = "shipments";
        } catch (e) {
            alert("Failed to create shipment: " + e.message);
        } finally {
            processingPickings.delete(pickingId);
            processingPickings = processingPickings;
        }
    }

    async function cancelShipment(pickingId) {
        if (!confirm("Are you sure you want to cancel this shipment?")) return;
        try {
            await api.post(`/api/delivery/shipments/${pickingId}/cancel`);
            await loadData();
        } catch (e) {
            alert("Failed to cancel shipment: " + e.message);
        }
    }

    async function syncOpal() {
        isSyncingOpal = true;
        toastStore.add("Syncing with OPAL...", "info");
        try {
            await api.post("/api/delivery/import/opal", {});
            toastStore.add("OPAL sync started. Check Scrapers page for details.", "success");
            setTimeout(async () => { await loadData(); isSyncingOpal = false; }, 4000);
        } catch (e) {
            toastStore.add("OPAL sync failed: " + e.message, "error");
            isSyncingOpal = false;
        }
    }

    async function syncDhl() {
        isSyncingDhl = true;
        toastStore.add("Syncing with DHL...", "info");
        try {
            await api.post("/api/delivery/import/dhl", {});
            toastStore.add("DHL sync started. Check Scrapers page for details.", "success");
            setTimeout(async () => { await loadData(); isSyncingDhl = false; }, 4000);
        } catch (e) {
            toastStore.add("DHL sync failed: " + e.message, "error");
            isSyncingDhl = false;
        }
    }

    function getProvider(details) {
        if (!details) return "unknown";
        if (details.provider === "dhl") return "dhl";
        if (details.ocu_number || details.hwb_number) return "opal";
        if (details.product?.includes("DHL")) return "dhl";
        return "opal";
    }

    function formatDate(dateStr) {
        if (!dateStr) return "-";
        return new Date(dateStr).toLocaleDateString("de-DE", {
            day: "2-digit", month: "2-digit", year: "numeric",
            hour: "2-digit", minute: "2-digit",
        });
    }

    function getStateColor(state) {
        const c = { draft: "#6c757d", assigned: "#ffc107", confirmed: "#17a2b8", done: "#28a745", cancel: "#dc3545" };
        return c[state] || "#6c757d";
    }

    function getDeliveryStateColor(state) {
        const c = { pending: "#ffc107", processing: "#17a2b8", shipped: "#28a745", delivered: "#28a745", failed: "#dc3545", cancelled: "#6c757d" };
        return c[state] || "#6c757d";
    }

    function toggleShipmentDetails(id) {
        if (expandedShipments.has(id)) expandedShipments.delete(id);
        else expandedShipments.add(id);
        expandedShipments = expandedShipments;
    }

    function formatDeliveryDate(statusDate, statusTime) {
        if (!statusDate) return null;
        let d;
        if (statusDate.includes('T')) {
            // DHL ISO format: "2026-02-20T17:25:16.508"
            d = new Date(statusDate);
        } else if (statusDate.includes('.')) {
            // OPAL format: "19.02.26" (DD.MM.YY) + statusTime "11:54"
            const parts = statusDate.split('.');
            if (parts.length === 3) {
                let [dd, mm, yy] = parts;
                const year = yy.length === 2 ? '20' + yy : yy;
                const timeStr = statusTime || '00:00';
                d = new Date(`${year}-${mm}-${dd}T${timeStr}:00`);
            }
        }
        if (!d || isNaN(d.getTime())) return statusDate + (statusTime ? ' ' + statusTime : '');
        return d.toLocaleString('de-DE', {
            day: '2-digit', month: '2-digit', year: 'numeric',
            hour: '2-digit', minute: '2-digit'
        });
    }

    function parseRawResponse(raw) {
        if (!raw) return null;
        try { return JSON.parse(raw); } catch { return null; }
    }

    // ── AI Match ──────────────────────────────────────────────────────────────
    let aiMatchLoading = {};  // keyed by shipment id
    let aiMatchResults = {};  // keyed by shipment id

    async function findTicketAI(shipmentId) {
        aiMatchLoading[shipmentId] = true;
        aiMatchLoading = aiMatchLoading;
        try {
            const matches = await api.get(`/api/delivery/shipments/${shipmentId}/ai-match`);
            aiMatchResults[shipmentId] = matches || [];
        } catch (e) {
            toastStore.add('AI match failed: ' + e.message, 'error');
            aiMatchResults[shipmentId] = [];
        } finally {
            aiMatchLoading[shipmentId] = false;
            aiMatchLoading = aiMatchLoading;
            aiMatchResults = aiMatchResults;
        }
    }

    function linkAndCreateRepair(shipment, details, match_) {
        const tracking = shipment.tracking_number || details?.ocu_number || details?.tracking_number || '';
        const params = new URLSearchParams({
            ticketId: match_.ticket_id,
            tracking,
            name: match_.customer_name || '',
            issue: match_.subject || '',
        });
        goto(`${base}/dashboard/repairs/new?${params}`);
    }

    function createRepairFromShipment(shipment, details) {
        const tracking = shipment.tracking_number || details?.ocu_number || details?.tracking_number || '';
        const customer = details?.pickup_name || details?.sender_name || '';
        const issue = tracking ? `Package received. Tracking: ${tracking}` : 'Package received.';

        const params = new URLSearchParams({
            tracking,
            name: customer,
            issue
        });
        goto(`${base}/dashboard/repairs/new?${params}`);
    }
</script>

<div class="shipping-page">
    <header>
        <h1>📦 Shipping & Delivery</h1>
        <div class="header-actions">
            {#if providersConfig?.opal === true}
                <button class="action-btn opal-btn" on:click={syncOpal} disabled={isSyncingOpal || loading}>
                    {isSyncingOpal ? "⏳ Syncing..." : "🟢 Sync OPAL"}
                </button>
            {/if}
            {#if providersConfig?.dhl === true}
                <button class="action-btn dhl-btn" on:click={syncDhl} disabled={isSyncingDhl || loading}>
                    {isSyncingDhl ? "⏳ Syncing..." : "🟡 Sync DHL"}
                </button>
            {/if}
            <button class="refresh-btn" on:click={loadData} disabled={loading}>
                {loading ? "↻ Loading..." : "↻ Refresh"}
            </button>
        </div>
    </header>

    <div class="tabs">
        <button class="tab" class:active={activeTab === "pickings"} on:click={() => (activeTab = "pickings")}>
            📋 Ready to Ship ({pickings.length})
        </button>
        <button class="tab" class:active={activeTab === "shipments"} on:click={() => (activeTab = "shipments")}>
            🚚 Shipments ({shipments.length})
        </button>
    </div>

    {#if loading && pickings.length === 0 && shipments.length === 0}
        <div class="loading">Loading shipping data...</div>
    {:else if error}
        <div class="error">Failed to load data: {error}</div>
    {:else if activeTab === "pickings"}
        <div class="pickings-section">
            <p class="section-desc">
                These are Odoo Transfer Orders ready to be shipped. Click "Ship with OPAL" to create a delivery shipment.
            </p>
            {#if pickings.length === 0}
                <div class="empty-state">
                    <p>✅ No pickings ready to ship</p>
                    <small>Pickings with status "assigned" will appear here</small>
                </div>
            {:else}
                <div class="table-container">
                    <table>
                        <thead>
                            <tr>
                                <th>Picking #</th><th>Origin</th><th>Partner</th>
                                <th>Location</th><th>State</th><th>Scheduled</th><th>Actions</th>
                            </tr>
                        </thead>
                        <tbody>
                            {#each pickings as picking}
                                <tr>
                                    <td class="picking-name">{picking.name}</td>
                                    <td>{picking.origin || "-"}</td>
                                    <td>{picking.partner_id || "-"}</td>
                                    <td>
                                        <div class="location-cell">
                                            <span class="from">{picking.location_id || "-"}</span>
                                            <span class="arrow">→</span>
                                            <span class="to">{picking.location_dest_id || "-"}</span>
                                        </div>
                                    </td>
                                    <td>
                                        <span class="state-badge" style="background-color: {getStateColor(picking.state)}">
                                            {picking.state}
                                        </span>
                                    </td>
                                    <td>{formatDate(picking.scheduled_date)}</td>
                                    <td>
                                        <button class="action-btn ship-btn" on:click={() => createShipment(picking.id)} disabled={processingPickings.has(picking.id)}>
                                            {processingPickings.has(picking.id) ? "⏳ Processing..." : "🚚 Ship with OPAL"}
                                        </button>
                                    </td>
                                </tr>
                            {/each}
                        </tbody>
                    </table>
                </div>
            {/if}
        </div>

    {:else if activeTab === "shipments"}
        <div class="shipments-section">
            <p class="section-desc">Active and past shipments created through the delivery system.</p>
            {#if shipments.length === 0}
                <div class="empty-state">
                    <p>📭 No shipments yet</p>
                    <small>Create your first shipment from the "Ready to Ship" tab</small>
                </div>
            {:else}
                <div class="table-container">
                    <table>
                        <thead>
                            <tr>
                                <th></th><th>Tracking</th><th>From → To</th>
                                <th>Product</th><th>Status</th><th>Delivered</th><th>Actions</th>
                            </tr>
                        </thead>
                        <tbody>
                            {#each shipments as shipment}
                                {@const details = parseRawResponse(shipment.raw_response)}
                                {@const provider = getProvider(details)}
                                <tr
                                    class="shipment-row"
                                    class:expanded={expandedShipments.has(shipment.id)}
                                    on:click={() => toggleShipmentDetails(shipment.id)}
                                >
                                    <td class="expand-cell">
                                        <span class="expand-icon">{expandedShipments.has(shipment.id) ? "▼" : "▶"}</span>
                                    </td>
                                    <td class="tracking-cell">
                                        <span class="provider-badge" class:opal={provider === "opal"} class:dhl={provider === "dhl"}>
                                            {provider.toUpperCase()}
                                        </span>
                                        {#if shipment.tracking_number || details?.ocu_number || details?.tracking_number}
                                            <span class="tracking-number">{shipment.tracking_number || details?.ocu_number || details?.tracking_number}</span>
                                            {#if details?.hwb_number}<span class="hwb-number">HWB: {details.hwb_number}</span>{/if}
                                        {:else}
                                            <span class="muted">Pending...</span>
                                        {/if}
                                    </td>
                                    <td>
                                        {#if details}
                                            <div class="route">
                                                <span class="from">{details.pickup_name || details.pickup_city || "InBody"}</span>
                                                <span class="arrow">→</span>
                                                <span class="to">{details.delivery_name || details.recipient_name || details.delivery_city || details.recipient_city || "-"}</span>
                                            </div>
                                        {:else}
                                            <span class="muted">-</span>
                                        {/if}
                                    </td>
                                    <td>
                                        {#if details?.product_type || details?.product}
                                            <span class="product-badge">{details.product_type || details.product}</span>
                                        {:else}<span class="muted">-</span>{/if}
                                    </td>
                                    <td>
                                        <span class="state-badge" style="background-color: {getDeliveryStateColor(shipment.status)}">
                                            {shipment.status}
                                        </span>
                                    </td>
                                    <td>
                                        {#if details?.status_date}
                                            <div class="delivery-info">
                                                <span>{formatDeliveryDate(details.status_date, details.status_time)}</span>
                                                {#if details.receiver}<span class="receiver">📝 {details.receiver}</span>{/if}
                                            </div>
                                        {:else}<span class="muted">-</span>{/if}
                                    </td>
                                    <td on:click|stopPropagation>
                                        <div class="actions-col">
                                            {#if shipment.status === "pending" || shipment.status === "processing"}
                                                <button class="action-btn cancel-btn" on:click={() => cancelShipment(shipment.picking_id)}>Cancel</button>
                                            {/if}
                                            <button class="action-btn repair-btn" on:click={() => createRepairFromShipment(shipment, details)}>Repair</button>
                                            <button
                                                class="action-btn ai-match-btn"
                                                on:click={() => { if (!expandedShipments.has(shipment.id)) toggleShipmentDetails(shipment.id); findTicketAI(shipment.id); }}
                                                disabled={aiMatchLoading[shipment.id]}
                                            >
                                                {aiMatchLoading[shipment.id] ? '...' : 'AI Match'}
                                            </button>
                                        </div>
                                    </td>
                                </tr>
                                {#if expandedShipments.has(shipment.id) && details}
                                    <tr class="details-row">
                                        <td colspan="7">
                                            <div class="shipment-details">
                                                <div class="details-grid">
                                                    <div class="detail-section">
                                                        <h4>📦 Pickup (Abholung)</h4>
                                                        <div class="detail-item"><label>Company:</label><span>{details.pickup_name || "-"}</span></div>
                                                        <div class="detail-item"><label>Address:</label><span>{details.pickup_street || "-"}, {details.pickup_zip} {details.pickup_city}</span></div>
                                                    </div>
                                                    <div class="detail-section">
                                                        <h4>🚚 Delivery (Zustellung)</h4>
                                                        <div class="detail-item"><label>Company:</label><span>{details.delivery_name || "-"}</span></div>
                                                        <div class="detail-item"><label>Address:</label><span>{details.delivery_street || "-"}, {details.delivery_zip} {details.delivery_city}</span></div>
                                                    </div>
                                                    <div class="detail-section">
                                                        <h4>📋 Package Info</h4>
                                                        {#if details.description}<div class="detail-item"><label>Contents:</label><span class="highlight">{details.description}</span></div>{/if}
                                                        {#if details.weight}<div class="detail-item"><label>Weight:</label><span>{details.weight} kg</span></div>{/if}
                                                    </div>
                                                </div>
                                                {#if aiMatchLoading[shipment.id] || aiMatchResults[shipment.id]?.length > 0}
                                                    <div class="ai-match-panel">
                                                        <h4>AI Ticket Matches</h4>
                                                        {#if aiMatchLoading[shipment.id]}
                                                            <div class="ai-match-loading">Searching for matching tickets...</div>
                                                        {:else}
                                                            <div class="ai-match-list">
                                                                {#each aiMatchResults[shipment.id] as match_}
                                                                    <div class="ai-match-item">
                                                                        <a href="{base}/dashboard/support/{match_.ticket_id}" class="match-info">
                                                                            <span class="match-ticket">#{match_.ticket_number || match_.ticket_id.substring(0,8)}</span>
                                                                            <span class="match-subject">{match_.subject}</span>
                                                                            <span class="match-customer">{match_.customer_name}</span>
                                                                            <span class="match-score">{Math.round(match_.similarity * 100)}%</span>
                                                                        </a>
                                                                        <button class="action-btn link-repair-btn" on:click|stopPropagation={() => linkAndCreateRepair(shipment, details, match_)}>
                                                                            Link & Create Repair
                                                                        </button>
                                                                    </div>
                                                                {/each}
                                                            </div>
                                                        {/if}
                                                    </div>
                                                {/if}
                                            </div>
                                        </td>
                                    </tr>
                                {/if}
                            {/each}
                        </tbody>
                    </table>
                </div>
            {/if}
        </div>
    {/if}
</div>

<style>
    .shipping-page { padding: 0; }
    header { display: flex; justify-content: space-between; align-items: center; margin-bottom: 1.5rem; }
    h1 { font-size: 1.8rem; color: #fff; margin: 0; }
    .header-actions { display: flex; gap: 1rem; }

    .refresh-btn { padding: 0.6rem 1.2rem; border-radius: 4px; border: 1px solid #4a69bd; background: transparent; color: #4a69bd; font-weight: 600; cursor: pointer; transition: all 0.2s; }
    .refresh-btn:hover:not(:disabled) { background: #4a69bd; color: white; }
    .refresh-btn:disabled { opacity: 0.5; cursor: not-allowed; }

    .action-btn { padding: 0.5rem 1rem; border-radius: 4px; border: none; font-weight: 600; font-size: 0.85rem; cursor: pointer; transition: all 0.2s; white-space: nowrap; }
    .action-btn.opal-btn { background: #1a472a; color: #4ade80; border: 1px solid #22c55e; }
    .action-btn.opal-btn:hover:not(:disabled) { background: #166534; }
    .action-btn.dhl-btn { background: #422006; color: #fbbf24; border: 1px solid #f59e0b; }
    .action-btn.dhl-btn:hover:not(:disabled) { background: #713f12; }
    .ship-btn { background: #28a745; color: white; }
    .ship-btn:hover:not(:disabled) { background: #218838; }
    .ship-btn:disabled { background: #555; cursor: not-allowed; opacity: 0.6; }
    .cancel-btn { background: transparent; border: 1px solid #dc3545; color: #dc3545; }
    .cancel-btn:hover { background: #dc3545; color: white; }

    .actions-col { display: flex; gap: 0.5rem; flex-wrap: wrap; }
    .repair-btn { background: transparent; border: 1px solid #3b82f6; color: #93c5fd; }
    .repair-btn:hover { background: #1e3a5f; color: #fff; }

    .tabs { display: flex; gap: 1rem; margin-bottom: 2rem; border-bottom: 2px solid #333; }
    .tab { padding: 0.8rem 1.5rem; border: none; background: transparent; color: #aaa; font-size: 1rem; font-weight: 600; cursor: pointer; border-bottom: 3px solid transparent; transition: all 0.2s; }
    .tab:hover { color: #fff; }
    .tab.active { color: #4a69bd; border-bottom-color: #4a69bd; }

    .section-desc { color: #aaa; margin-bottom: 1.5rem; font-size: 0.95rem; }
    .loading, .error { text-align: center; padding: 3rem; color: #666; background: #1e1e1e; border-radius: 8px; border: 1px solid #333; }
    .error { color: #ff6b6b; border-color: #ff6b6b; }
    .empty-state { text-align: center; padding: 3rem; color: #666; background: #1e1e1e; border-radius: 8px; border: 1px dashed #333; }
    .empty-state p { font-size: 1.2rem; margin: 0 0 0.5rem 0; }
    .empty-state small { color: #555; }

    .table-container { background: #1e1e1e; border-radius: 8px; border: 1px solid #333; overflow-x: auto; }
    table { width: 100%; border-collapse: collapse; }
    thead { background: #252525; }
    th { padding: 1rem; text-align: left; font-weight: 600; color: #aaa; text-transform: uppercase; font-size: 0.75rem; letter-spacing: 0.5px; border-bottom: 2px solid #333; }
    td { padding: 1rem; border-bottom: 1px solid #2a2a2a; color: #e0e0e0; }
    tbody tr:hover { background: #252525; }
    .picking-name, .tracking-number { font-family: monospace; color: #4a69bd; font-weight: 600; }

    .location-cell, .route { display: flex; align-items: center; gap: 0.5rem; }
    .arrow { color: #666; }
    .from { color: #ffc107; font-weight: 500; }
    .to { color: #28a745; font-weight: 500; }

    .state-badge { display: inline-block; padding: 0.3rem 0.8rem; border-radius: 12px; font-size: 0.75rem; font-weight: 600; text-transform: uppercase; color: white; }
    .provider-badge { display: inline-block; padding: 0.3rem 0.8rem; border-radius: 4px; background: #2a2a2a; font-family: monospace; font-size: 0.85rem; text-transform: uppercase; color: #4a69bd; margin-right: 0.5rem; }
    .provider-badge.opal { background: #166534; color: #4ade80; }
    .provider-badge.dhl { background: #713f12; color: #fbbf24; }
    .muted { color: #666; font-style: italic; }

    .shipment-row { cursor: pointer; transition: background 0.2s; }
    .shipment-row:hover { background: #2a2a2a; }
    .shipment-row.expanded { background: #252525; border-bottom: none; }
    .expand-cell { width: 30px; text-align: center; }
    .expand-icon { color: #666; font-size: 0.8rem; }
    .tracking-cell { display: flex; flex-direction: column; gap: 0.2rem; }
    .hwb-number { font-family: monospace; font-size: 0.75rem; color: #888; }
    .product-badge { display: inline-block; padding: 0.2rem 0.6rem; border-radius: 4px; background: #2a2a2a; font-size: 0.8rem; color: #aaa; }
    .delivery-info { display: flex; flex-direction: column; gap: 0.2rem; }
    .receiver { font-size: 0.8rem; color: #28a745; }

    .details-row { background: #1a1a1a; }
    .details-row td { padding: 0; border-bottom: 2px solid #333; }
    .shipment-details { padding: 1.5rem; }
    .details-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(280px, 1fr)); gap: 1.5rem; }
    .detail-section { background: #252525; border-radius: 8px; padding: 1rem; }
    .detail-section h4 { margin: 0 0 1rem 0; color: #fff; font-size: 0.95rem; border-bottom: 1px solid #333; padding-bottom: 0.5rem; }
    .detail-item { display: flex; gap: 0.5rem; margin-bottom: 0.5rem; font-size: 0.9rem; }
    .detail-item label { color: #888; min-width: 80px; flex-shrink: 0; }
    .detail-item span { color: #e0e0e0; }
    .detail-item .highlight { color: #4a69bd; font-weight: 600; }

    /* AI Match */
    .ai-match-btn { background: #1a1a3a; color: #a78bfa; border: 1px solid #7c3aed; }
    .ai-match-btn:hover:not(:disabled) { background: #2a1a4a; }
    .ai-match-btn:disabled { opacity: 0.5; cursor: not-allowed; }

    .ai-match-panel {
        margin-top: 1.5rem;
        background: #0f1a2e;
        border: 1px solid #1e40af;
        border-radius: 8px;
        padding: 1rem 1.25rem;
    }
    .ai-match-panel h4 { margin: 0 0 0.75rem 0; color: #60a5fa; font-size: 0.85rem; text-transform: uppercase; letter-spacing: 0.5px; }
    .ai-match-loading { color: #888; font-style: italic; font-size: 0.85rem; }
    .ai-match-list { display: flex; flex-direction: column; gap: 0.5rem; }
    .ai-match-item {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: 0.75rem;
        background: #1a2744;
        border: 1px solid #1e3a5f;
        border-radius: 6px;
        padding: 0.5rem 0.75rem;
    }
    .match-info {
        display: flex;
        align-items: center;
        gap: 0.75rem;
        flex: 1;
        text-decoration: none;
        color: #ccc;
        min-width: 0;
        overflow: hidden;
    }
    .match-info:hover { color: #fff; }
    .match-ticket { font-family: monospace; color: #6bc5f0; font-weight: 600; flex-shrink: 0; }
    .match-subject { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 0.85rem; }
    .match-customer { color: #888; font-size: 0.8rem; flex-shrink: 0; }
    .match-score { font-family: monospace; color: #60a5fa; font-size: 0.8rem; flex-shrink: 0; }
    .link-repair-btn { background: #1a3a1a; color: #4ade80; border: 1px solid #22c55e; font-size: 0.78rem; flex-shrink: 0; }
    .link-repair-btn:hover { background: #14532d; }
</style>
