<script>
    import { page } from '$app/stores';
    import { onMount } from 'svelte';
    import { api } from '$lib/api';
    import { goto, afterNavigate } from '$app/navigation';
    import { base } from '$app/paths';
    import { toastStore } from '$lib/stores/toastStore';
    import { t, tr } from '$lib/i18n';
    // [MODULE: GEO_ROUTING START]
    import GeoRouteMap from '$lib/components/GeoRouteMap.svelte';
    // [MODULE: GEO_ROUTING END]

    let orderId = $page.params.id;
    let isNew = orderId === 'new';
    let loading = !isNew;

    // Form Data
    let formData = {
        orderType: 'repair',
        orderNumber: '',
        customerName: '',
        customerEmail: '',
        productSku: '',
        productName: '',
        serialNumber: '',
        issueDescription: '',
        status: 'pending',
        priority: 'normal',
        repairNotes: '',
        laborHours: 0,
        partsUsed:[],
        metadata: {},
    };

    // System keys to hide from dynamic attributes list
    const hiddenMetaKeys =['ticketId', 'trackingNumber', 'importedFromExcel', 'excelRow'];

    // State for new custom fields
    let newFieldKey = '';
    let newFieldValue = '';
    let newPart = '';

    // Similar repairs search
    let similarRepairs = [];
    let isSearchingSimilar = false;
    let hasSearched = false;

    async function findSimilar() {
        if (!formData.issueDescription || formData.issueDescription.trim().length < 3) {
            toastStore.add(tr('repairs.issue_too_short'), 'warning');
            return;
        }
        isSearchingSimilar = true;
        hasSearched = true;
        similarRepairs = [];
        try {
            const results = await api.post('/api/rma/search', { query: formData.issueDescription });
            similarRepairs = results.filter(r => r.order_number !== formData.orderNumber);
        } catch (e) {
            toastStore.add(tr('repairs.search_failed', { error: e.message }), 'error');
        } finally {
            isSearchingSimilar = false;
        }
    }
    // Clickwrap Agreement
    async function generateAgreementLink() {
        try {
            const res = await api.post(`/api/rma/${orderId}/generate-link`, {});
            formData.agreement_status = 'sent';
            formData.agreement_token = res.token;
            formData.agreement_url = res.url;
            toastStore.add(tr('repairs.link_generated'), 'success');
        } catch (e) {
            toastStore.add(tr('repairs.link_generate_failed', { error: e.message }), 'error');
        }
    }

    async function copyAgreementLink() {
        const url = formData.agreement_url || `${window.location.origin}${base}/sign/${formData.agreement_token}`;
        try {
            await navigator.clipboard.writeText(url);
            toastStore.add(tr('repairs.link_copied'), 'success');
        } catch(e) {
            toastStore.add(tr('repairs.copy_failed'), 'error');
        }
    }

    // [MODULE: GEO_ROUTING START]
    let showMap = false;
    let geocoding = false;

    async function openRouteMap() {
        if (!formData.metadata?.geo) {
            geocoding = true;
            try {
                const geo = await api.post(`/api/geo/geocode/${orderId}`);
                formData.metadata = { ...formData.metadata, geo: { lat: geo.lat, lng: geo.lng } };
            } catch (e) {
                toastStore.add(tr('repairs.geocoding_failed', { error: e.message || tr('repairs.unknown_error') }), 'error');
                geocoding = false;
                return;
            }
            geocoding = false;
        }
        showMap = true;
    }
    // [MODULE: GEO_ROUTING END]

    onMount(async () => {
        if (!isNew) {
            await loadRepair();
        } else {
            formData.orderNumber = 'AUTO-GEN';
            // Pre-fill from URL params when coming from a Support ticket
            const params = $page.url.searchParams;
            const linkedTicketId = params.get('ticketId');
            const linkedTracking = params.get('tracking');

            if (linkedTicketId) {
                formData.metadata = { ...formData.metadata, ticketId: linkedTicketId };
                formData.customerName     = params.get('name')  || '';
                formData.customerEmail    = params.get('email') || '';
                formData.issueDescription = params.get('issue') || '';
            }
            if (linkedTracking) {
                formData.metadata = { ...formData.metadata, trackingNumber: linkedTracking };
                if (!formData.customerName) formData.customerName = params.get('name') || '';
                if (!formData.issueDescription) formData.issueDescription = params.get('issue') || '';
            }

            const linkedSerial = params.get('serial');
            const linkedModel = params.get('model');
            if (linkedSerial) formData.serialNumber = linkedSerial;
            if (linkedModel) formData.productSku = linkedModel;
        }
    });

    async function loadRepair() {
        try {
            const data = await api.get(`/api/rma/${orderId}`);
            formData = { ...data };
            if (!Array.isArray(formData.partsUsed)) {
                formData.partsUsed =[];
            }
        } catch (e) {
            toastStore.add(tr('repairs.error_loading'), 'error');
            goto(`${base}/dashboard/repairs`);
        } finally {
            loading = false;
        }
    }

    async function handleSubmit() {
        try {
            if (isNew) {
                if (!formData.customerName || !formData.productSku) {
                    toastStore.add(tr('repairs.customer_sku_required'), 'warning');
                    return;
                }
                if (formData.orderNumber === 'AUTO-GEN') delete formData.orderNumber;
                formData.orderType = 'repair';
                formData.laborHours = parseFloat(formData.laborHours) || 0;
                await api.post('/api/rma', formData);
                toastStore.add(tr('repairs.created_success'), 'success');
            } else {
                formData.laborHours = parseFloat(formData.laborHours) || 0;
                await api.put(`/api/rma/${orderId}`, formData);
                toastStore.add(tr('repairs.updated_success'), 'success');
            }
            goto(`${base}/dashboard/repairs`);
        } catch (e) {
            toastStore.add(tr('repairs.error_generic', { error: e.message }), 'error');
        }
    }

    async function deleteRepair() {
        if (!confirm(tr('repairs.delete_confirm'))) return;
        try {
            await api.delete(`/api/rma/${orderId}`);
            toastStore.add(tr('repairs.deleted_success'), 'success');
            goto(`${base}/dashboard/repairs`);
        } catch (e) {
            toastStore.add(e.message, 'error');
        }
    }

    // Remember where we came from (map / repairs list / related) so "← Back"
    // returns there, not always to the list root.
    let backTo = null;
    afterNavigate((nav) => {
        if (nav.from?.url) backTo = nav.from.url.pathname + nav.from.url.search;
    });
    function goBack() {
        if (backTo) goto(backTo);
        else goto(`${base}/dashboard/repairs`);
    }
    function goUp() {
        goto(`${base}/dashboard/repairs`);
    }

    // --- Dynamic Fields Logic ---

    function formatKey(key) {
        // camelCase to Title Case
        const result = key.replace(/([A-Z])/g, ' $1');
        return result.charAt(0).toUpperCase() + result.slice(1);
    }

    function addPart() {
        if (!newPart.trim()) return;
        formData.partsUsed =[...formData.partsUsed, newPart.trim()];
        newPart = '';
    }

    function removePart(index) {
        formData.partsUsed = formData.partsUsed.filter((_, i) => i !== index);
    }

    function addCustomField() {
        if (!newFieldKey.trim()) return;
        let key = newFieldKey.trim().replace(/\s+/g, '_');
        // Simple type inference
        let val = newFieldValue;
        if (val.toLowerCase() === 'true') val = true;
        if (val.toLowerCase() === 'false') val = false;
        if (!isNaN(val) && val !== '') val = Number(val);

        formData.metadata = { ...formData.metadata, [key]: val };
        newFieldKey = '';
        newFieldValue = '';
    }

    function updateMetadata(key, value) {
        formData.metadata = { ...formData.metadata, [key]: value };
    }

    function updateNestedMetadata(parentKey, childKey, value) {
        formData.metadata = {
            ...formData.metadata,
            [parentKey]: {
                ...formData.metadata[parentKey],
                [childKey]: value
            }
        };
    }
</script>

<div class="detail-page">
    <div class="header">
        <button class="back-btn" on:click={goBack} title={$t('repairs.back_title')}>{$t('repairs.back')}</button>
        <button class="back-btn back-btn-up" on:click={goUp} title={$t('repairs.all_repairs_title')}>{$t('repairs.all_repairs')}</button>
        <div class="title-row">
            <h1>{isNew ? $t('repairs.new_order_title') : $t('repairs.order_title', { number: formData.orderNumber })}</h1>
            {#if !isNew}
                <!-- [MODULE: GEO_ROUTING START] -->
                <button class="route-btn" on:click={openRouteMap} disabled={geocoding}>
                    {geocoding ? $t('repairs.geocoding_btn') : $t('repairs.view_route_btn')}
                </button>
                <!-- [MODULE: GEO_ROUTING END] -->
                <button class="delete-btn" on:click={deleteRepair}>{$t('repairs.delete')}</button>
            {/if}
        </div>
    </div>

    {#if loading}
        <div class="loading">{$t('repairs.loading')}</div>
    {:else}
        <form class="form-grid" on:submit|preventDefault={handleSubmit}>
            {#if formData.metadata?.ticketId || formData.metadata?.trackingNumber}
                <div class="section full linked-banner">
                    <div class="linked-row">
                        {#if formData.metadata?.ticketId}
                            <span class="linked-label">{$t('repairs.linked_support_ticket')}</span>
                            <a class="linked-link" href="{base}/dashboard/support/{formData.metadata.ticketId}">
                                {$t('repairs.view_ticket_link', { id: formData.metadata.ticketId })}
                            </a>
                        {/if}
                        {#if formData.metadata?.trackingNumber}
                            <span class="linked-label" style="margin-left: 1rem;">{$t('repairs.linked_shipment')}</span>
                            <span class="linked-link" style="border-bottom: none; color: #fff; cursor: default;">
                                {formData.metadata.trackingNumber}
                            </span>
                        {/if}
                    </div>
                </div>
            {/if}

            <div class="section">
                <h2>{$t('repairs.customer_information')}</h2>
                <div class="field">
                    <label>{$t('repairs.customer_name_label')}</label>
                    <input type="text" bind:value={formData.customerName} required />
                </div>
                <div class="field">
                    <label>{$t('repairs.email_label')}</label>
                    <input type="email" bind:value={formData.customerEmail} />
                </div>
            </div>

            <div class="section">
                <h2>{$t('repairs.device_details')}</h2>
                <div class="field">
                    <label>{$t('repairs.device_model_sku_label')}</label>
                    <input type="text" bind:value={formData.productSku} required class="code-input" />
                </div>
                <div class="field">
                    <label>{$t('repairs.serial_number_label')}</label>
                    <input type="text" bind:value={formData.serialNumber} class="code-input" />
                </div>
            </div>

            <div class="section full">
                <div class="section-header">
                    <h2>{$t('repairs.issue_description')}</h2>
                    <button type="button" class="btn secondary btn-sm" on:click={findSimilar} disabled={isSearchingSimilar || isNew}>
                        {#if isSearchingSimilar}
                            <span class="spinner">&#8635;</span> {$t('repairs.searching')}
                        {:else}
                            {$t('repairs.find_similar')}
                        {/if}
                    </button>
                </div>
                <textarea bind:value={formData.issueDescription} rows="3"></textarea>

                {#if hasSearched}
                    <div class="similar-results">
                        <h3>{$t('repairs.similar_historical')}</h3>
                        {#if similarRepairs.length === 0}
                            <p class="muted">{$t('repairs.no_similar')}</p>
                        {:else}
                            <div class="similar-grid">
                                {#each similarRepairs as rep}
                                    <a class="similar-card" href="{base}/dashboard/repairs/{rep.id}" target="_blank">
                                        <div class="sim-header">
                                            <span class="sim-id">{rep.order_number}</span>
                                            <span class="sim-score">{$t('repairs.percent_match', { percent: (rep.score * 100).toFixed(0) })}</span>
                                        </div>
                                        <div class="sim-body">
                                            <div class="sim-issue"><strong>{$t('repairs.issue_label')}</strong> {rep.issue_description || $t('repairs.na')}</div>
                                            <div class="sim-reso"><strong>{$t('repairs.resolution_label')}</strong> {rep.resolution || $t('repairs.pending_value')}</div>
                                        </div>
                                    </a>
                                {/each}
                            </div>
                        {/if}
                    </div>
                {/if}
            </div>

            <div class="section">
                <h2>{$t('repairs.repair_details')}</h2>
                <div class="field">
                    <label>{$t('repairs.labor_hours_label')}</label>
                    <input type="number" step="0.1" min="0" bind:value={formData.laborHours} />
                </div>
                <div class="field">
                    <label>{$t('repairs.repair_notes_label')}</label>
                    <textarea bind:value={formData.repairNotes} rows="4"></textarea>
                </div>
            </div>

            <div class="section">
                <h2>{$t('repairs.status_priority')}</h2>
                <div class="field">
                    <label>{$t('repairs.status_label')}</label>
                    <select bind:value={formData.status}>
                        <option value="pending">{$t('repairs.status_pending')}</option>
                        <option value="received">{$t('repairs.status_received')}</option>
                        <option value="processing">{$t('repairs.status_processing')}</option>
                        <option value="completed">{$t('repairs.status_completed')}</option>
                        <option value="cancelled">{$t('repairs.status_cancelled')}</option>
                    </select>
                </div>
                <div class="field">
                    <label>{$t('repairs.priority_label')}</label>
                    <select bind:value={formData.priority}>
                        <option value="low">{$t('repairs.priority_low')}</option>
                        <option value="normal">{$t('repairs.priority_normal')}</option>
                        <option value="high">{$t('repairs.priority_high')}</option>
                        <option value="urgent">{$t('repairs.priority_urgent')}</option>
                    </select>
                </div>
            </div>

            {#if !isNew}
                <div class="section">
                    <h2>{$t('repairs.legal_agreements')}</h2>
                    <div class="field">
                        <label>{$t('repairs.clickwrap_contract')}</label>
                        <div class="agreement-status-box">
                            {#if formData.agreement_status === 'signed'}
                                <span class="badge-success">{$t('repairs.legally_signed')}</span>
                                <div class="audit-info">
                                    <div><small><strong>{$t('repairs.ip_label')}</strong> {formData.agreement_log?.audit_log?.ip_address || $t('repairs.na')}</small></div>
                                    <div><small><strong>{$t('repairs.time_label')}</strong> {formData.agreement_log?.audit_log?.timestamp ? new Date(formData.agreement_log.audit_log.timestamp).toLocaleString() : $t('repairs.na')}</small></div>
                                    {#if formData.agreement_log?.content_hash}
                                        <div><small><strong>{$t('repairs.hash_label')}</strong> <span class="mono">{formData.agreement_log.content_hash.substring(0, 16)}...</span></small></div>
                                    {/if}
                                </div>
                            {:else if formData.agreement_status === 'sent'}
                                <span class="badge-warning">{$t('repairs.pending_signature')}</span>
                                <button type="button" class="btn secondary btn-sm" on:click={copyAgreementLink} style="margin-top: 0.5rem;">{$t('repairs.copy_magic_link')}</button>
                            {:else}
                                <button type="button" class="btn primary btn-sm" on:click={generateAgreementLink}>{$t('repairs.generate_clickwrap_link')}</button>
                            {/if}
                        </div>
                    </div>
                </div>
            {/if}

            <!-- Replaced Parts Section -->
            <div class="section full">
                <div class="section-header">
                    <h2>{$t('repairs.replaced_parts')}</h2>
                </div>
                <div class="tags-container">
                    {#each formData.partsUsed as part, i}
                        <span class="part-tag">
                            {part}
                            <button type="button" class="remove-tag" on:click={() => removePart(i)}>&times;</button>
                        </span>
                    {/each}
                </div>
                <div class="add-tag-row">
                    <input type="text" bind:value={newPart} placeholder={$t('repairs.part_placeholder')} on:keydown={(e) => e.key === 'Enter' && (e.preventDefault(), addPart())} />
                    <button type="button" class="btn secondary" on:click={addPart}>{$t('repairs.add_part')}</button>
                </div>
            </div>

            <!-- Dynamic Attributes (Metadata) -->
            <div class="section full dynamic-section">
                <div class="section-header">
                    <h2>{$t('repairs.dynamic_attributes')}</h2>
                    <span class="badge metadata-badge">{$t('repairs.metadata_badge')}</span>
                </div>
                <p class="section-hint">{$t('repairs.dynamic_hint')}</p>

                <div class="dynamic-grid">
                    {#each Object.entries(formData.metadata || {}).filter(([k]) => !hiddenMetaKeys.includes(k)) as [key, value]}
                        {#if typeof value === 'object' && value !== null && !Array.isArray(value)}
                            <div class="nested-group">
                                <h4>{formatKey(key)}</h4>
                                <div class="nested-fields">
                                    {#each Object.entries(value) as [subKey, subVal]}
                                        <div class="field">
                                            <label>{formatKey(subKey)}</label>
                                            <input type="text" value={subVal} on:input={(e) => updateNestedMetadata(key, subKey, e.target.value)} />
                                        </div>
                                    {/each}
                                </div>
                            </div>
                        {:else if typeof value === 'boolean'}
                            <div class="field boolean-field">
                                <label class="checkbox-label">
                                    <input type="checkbox" checked={value} on:change={(e) => updateMetadata(key, e.target.checked)} />
                                    {formatKey(key)}
                                </label>
                            </div>
                        {:else}
                            <div class="field">
                                <label>{formatKey(key)}</label>
                                <input type="text" value={value} on:input={(e) => updateMetadata(key, e.target.value)} />
                            </div>
                        {/if}
                    {/each}
                </div>

                <!-- Add custom field -->
                <div class="add-custom-field">
                    <input type="text" bind:value={newFieldKey} placeholder={$t('repairs.new_field_key_placeholder')} />
                    <input type="text" bind:value={newFieldValue} placeholder={$t('repairs.value_placeholder')} on:keydown={(e) => e.key === 'Enter' && (e.preventDefault(), addCustomField())} />
                    <button type="button" class="btn secondary" on:click={addCustomField}>{$t('repairs.add_field')}</button>
                </div>
            </div>

            <div class="actions full">
                <button type="button" class="cancel-btn" on:click={goBack}>{$t('repairs.cancel')}</button>
                <button type="submit" class="save-btn">{isNew ? $t('repairs.create_order') : $t('repairs.save_changes')}</button>
            </div>
        </form>
    {/if}

    <!-- [MODULE: GEO_ROUTING START] -->
    {#if showMap && formData.metadata?.geo}
        <div class="map-overlay" on:click|self={() => showMap = false}>
            <div class="map-modal">
                <div class="map-modal-header">
                    <h2>{$t('repairs.route_nearby_tasks')}</h2>
                    <button class="close-btn" on:click={() => showMap = false}>&times;</button>
                </div>
                <GeoRouteMap
                    targetLat={formData.metadata.geo.lat}
                    targetLng={formData.metadata.geo.lng}
                    targetTitle={formData.orderNumber || $t('repairs.this_order')}
                />
            </div>
        </div>
    {/if}
    <!-- [MODULE: GEO_ROUTING END] -->
</div>

<style>
    .detail-page { max-width: 900px; margin: 0 auto; padding-bottom: 2rem; }
    .header { margin-bottom: 2rem; }
    .back-btn { background: none; border: none; color: #888; cursor: pointer; font-size: 1rem; padding: 0; margin-bottom: 1rem; }
    .back-btn:hover { color: #fff; }
    .back-btn-up { margin-left: 1.25rem; color: #888; }
    .back-btn-up:hover { color: #ccc; }
    .title-row { display: flex; justify-content: space-between; align-items: center; }
    h1 { color: #fff; font-size: 2rem; margin: 0; }
    h2 { color: #ccc; font-size: 1.1rem; margin-bottom: 1rem; border-bottom: 1px solid #333; padding-bottom: 0.5rem; }

    .form-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 1.5rem; }
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
    .section-header { display: flex; justify-content: space-between; align-items: center; border-bottom: 1px solid #333; padding-bottom: 0.5rem; margin-bottom: 0.5rem; }
    .section-header h2 { border-bottom: none; margin-bottom: 0; padding-bottom: 0; }
    .section-hint { color: #888; font-size: 0.85rem; margin: 0 0 1rem 0; font-style: italic; }

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
    input:focus, select:focus, textarea:focus { border-color: #4a69bd; outline: none; }
    input[type="checkbox"] { width: auto; margin-right: 0.5rem; transform: scale(1.2); }
    .checkbox-label { display: flex; align-items: center; color: #ccc; font-size: 0.95rem; cursor: pointer; }
    .code-input { font-family: monospace; }

    .btn { padding: 0.8rem 1.5rem; border-radius: 4px; border: none; font-weight: 600; cursor: pointer; font-size: 0.95rem; }
    .btn.secondary { background: #333; color: #ccc; }
    .btn.secondary:hover { background: #444; }

    /* Tags */
    .tags-container { display: flex; flex-wrap: wrap; gap: 0.5rem; margin-bottom: 0.5rem; }
    .part-tag { background: rgba(74, 105, 189, 0.2); color: #93c5fd; border: 1px solid #4a69bd; padding: 0.4rem 0.8rem; border-radius: 6px; font-size: 0.9rem; display: flex; align-items: center; gap: 0.5rem; }
    .remove-tag { background: none; border: none; color: #93c5fd; cursor: pointer; font-size: 1.1rem; padding: 0; line-height: 1; }
    .remove-tag:hover { color: #ff6b6b; }
    .add-tag-row { display: flex; gap: 0.5rem; }
    .add-tag-row input { flex: 1; }

    /* Dynamic Grid */
    .dynamic-section { background: rgba(30, 30, 30, 0.5); }
    .dynamic-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(280px, 1fr)); gap: 1rem; margin-bottom: 1.5rem; }
    .metadata-badge { background: #3a2a0a; color: #fbbf24; border: 1px solid #f59e0b; padding: 0.2rem 0.5rem; border-radius: 4px; font-size: 0.7rem; text-transform: uppercase; }
    .nested-group { grid-column: 1 / -1; background: #1a1a1a; padding: 1rem; border-radius: 6px; border: 1px solid #2a2a2a; }
    .nested-group h4 { margin: 0 0 1rem 0; color: #a3bffa; font-size: 0.9rem; text-transform: uppercase; }
    .nested-fields { display: grid; grid-template-columns: repeat(auto-fill, minmax(200px, 1fr)); gap: 1rem; }
    .boolean-field { justify-content: center; background: #1a1a1a; padding: 0.8rem; border-radius: 4px; border: 1px solid #2a2a2a; }
    .add-custom-field { display: flex; gap: 0.5rem; align-items: stretch; border-top: 1px dashed #444; padding-top: 1rem; }

    .actions { margin-top: 1rem; display: flex; justify-content: flex-end; gap: 1rem; }
    .save-btn { background: #28a745; color: white; border: none; padding: 0.8rem 2rem; border-radius: 4px; font-weight: 600; cursor: pointer; }
    .save-btn:hover { background: #218838; }
    .cancel-btn { background: #333; color: #ccc; border: none; padding: 0.8rem 1.5rem; border-radius: 4px; cursor: pointer; }
    .cancel-btn:hover { background: #444; }
    .delete-btn { background: #d32f2f; color: white; border: none; padding: 0.6rem 1.2rem; border-radius: 4px; cursor: pointer; }

    .linked-banner { background: #1a2a3a; border-color: #3b82f6; padding: 0.9rem 1.25rem; }
    .linked-row { display: flex; align-items: center; gap: 1rem; flex-wrap: wrap; }
    .linked-label { color: #93c5fd; font-weight: 600; font-size: 0.9rem; }
    .linked-link { color: #bfdbfe; text-decoration: none; font-family: monospace; font-size: 0.85rem; border-bottom: 1px dashed #4a69bd; }
    .linked-link:hover { color: #fff; border-bottom-color: #fff; }

    /* [MODULE: GEO_ROUTING START] */
    .route-btn { background: #4a69bd; color: #fff; border: none; padding: 0.6rem 1.2rem; border-radius: 4px; cursor: pointer; font-weight: 600; }
    .route-btn:hover:not(:disabled) { background: #3b5bdb; }
    .route-btn:disabled { opacity: 0.6; cursor: not-allowed; }

    .map-overlay {
        position: fixed; inset: 0; background: rgba(0,0,0,0.7); z-index: 9999;
        display: flex; align-items: center; justify-content: center;
    }
    .map-modal {
        background: #181818; border: 1px solid #333; border-radius: 10px;
        width: 90vw; max-width: 1100px; max-height: 90vh; overflow: hidden;
        padding: 1.25rem; display: flex; flex-direction: column; gap: 0.75rem;
    }
    .map-modal-header { display: flex; justify-content: space-between; align-items: center; }
    .map-modal-header h2 { color: #ccc; margin: 0; font-size: 1.1rem; }
    .close-btn { background: none; border: none; color: #888; font-size: 1.8rem; cursor: pointer; line-height: 1; }
    .close-btn:hover { color: #fff; }
    /* [MODULE: GEO_ROUTING END] */

    /* Agreement */
    .btn.primary { background: #4a69bd; color: #fff; }
    .btn.primary:hover { background: #3b5bdb; }
    .agreement-status-box { display: flex; flex-direction: column; gap: 0.3rem; }
    .badge-success { background: rgba(74, 222, 128, 0.15); color: #4ade80; border: 1px solid #4ade80; padding: 0.3rem 0.6rem; border-radius: 4px; font-size: 0.85rem; font-weight: 600; display: inline-block; margin-bottom: 0.5rem; }
    .badge-warning { background: rgba(251, 191, 36, 0.15); color: #fbbf24; border: 1px solid #f59e0b; padding: 0.3rem 0.6rem; border-radius: 4px; font-size: 0.85rem; font-weight: 600; display: inline-block; }
    .audit-info { font-size: 0.8rem; color: #888; display: flex; flex-direction: column; gap: 0.2rem; }

    /* Similar repairs search */
    .btn-sm { padding: 0.4rem 0.8rem; font-size: 0.8rem; }
    .similar-results { margin-top: 1rem; padding-top: 1rem; border-top: 1px dashed #444; }
    .similar-results h3 { font-size: 0.95rem; color: #a3bffa; margin: 0 0 0.8rem 0; text-transform: uppercase; }
    .similar-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(300px, 1fr)); gap: 1rem; }
    .similar-card { background: #1a1a1a; border: 1px solid #333; border-radius: 6px; padding: 0.8rem; text-decoration: none; display: flex; flex-direction: column; gap: 0.5rem; transition: border-color 0.2s; }
    .similar-card:hover { border-color: #4a69bd; }
    .sim-header { display: flex; justify-content: space-between; align-items: center; border-bottom: 1px solid #2a2a2a; padding-bottom: 0.4rem; }
    .sim-id { font-family: monospace; color: #fff; font-weight: bold; }
    .sim-score { font-size: 0.75rem; color: #4ade80; background: rgba(74, 222, 128, 0.1); padding: 2px 6px; border-radius: 4px; }
    .sim-body { font-size: 0.85rem; color: #ccc; display: flex; flex-direction: column; gap: 0.4rem; }
    .sim-issue, .sim-reso { display: -webkit-box; -webkit-line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; }
    .sim-reso { color: #93c5fd; }
    .muted { color: #666; font-style: italic; }
    .spinner { display: inline-block; animation: spin 1s linear infinite; }
    @keyframes spin { to { transform: rotate(360deg); } }

    @media (max-width: 700px) { .form-grid { grid-template-columns: 1fr; } .add-custom-field { flex-direction: column; } }
</style>
