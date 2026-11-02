<script>
    import { onMount } from 'svelte';
    import { api } from '$lib/api';
    import { goto } from '$app/navigation';
    import { base } from '$app/paths';
    import { toastStore } from '$lib/stores/toastStore';
    import { t, tr } from '$lib/i18n';
    import AuthImg from '$lib/components/AuthImg.svelte';

    // Leading icon column (ENTITY_PAGES_AND_SCAN_MIRROR §4): a representative
    // device photo per repair when the backend supplies `icon_file_id`,
    // otherwise the kind glyph (a wrench). /api/files is auth-gated, so the
    // thumbnail goes through AuthImg (Bearer-token fetch → object URL).
    function iconUrl(fileId) {
        return `${base}/api/files/${fileId}`;
    }

    // Kept-alive by the dashboard layout (mounted once, persists across nav).
    // `active` = this is the visible tab; accepted for a uniform cache API.
    export let active = true;

    let orders = [];
    let awaiting = [];
    let loading = true;
    let error = null;

    onMount(async () => {
        await loadRepairs();
    });

    async function loadRepairs() {
        try {
            // /api/repairs/board = repair orders (same shape as
            // /api/rma?type=repair, incl. icon_file_id) + the "awaiting triage"
            // column: inbound units received but not yet opened as an order.
            const data = await api.get('/api/repairs/board');
            orders = (data && data.orders) || [];
            awaiting = (data && data.awaiting) || [];
        } catch (e) {
            console.error(e);
            error = e.message;
            toastStore.add(tr('lists.repairs_load_failed'), 'error');
        } finally {
            loading = false;
        }
    }

    // `order.id` arrives as a record string like  order:`<uuid>`  -- the detail
    // route and /api/rma/:id want the bare uuid, so always navigate by the leaf.
    function leafId(order) {
        if (order.uuid) return order.uuid;
        const m = String(order.id || '').match(/([0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12})/);
        return m ? m[1] : order.id;
    }

    function openRepair(id) {
        goto(`${base}/dashboard/repairs/${id}`);
    }

    // An awaiting unit has no order yet — open its entity page instead.
    function openUnit(unitId) {
        goto(`${base}/dashboard/obj/unit/${unitId}`);
    }

    function receivedAgoText(unit) {
        const when = unit.received_at;
        if (!when) return '-';
        const days = Math.floor((Date.now() - new Date(when).getTime()) / 86_400_000);
        if (days <= 0) return tr('lists.received_today');
        return tr('lists.received_days_ago', { n: days });
    }

    // Company (requester firm) + city. The backend resolves `company` from the
    // unit's receipt ticket (falling back to the receipt client_name); the
    // requester PERSON is never sent. No order/GER number is shown.
    function awaitingWhere(unit) {
        return [unit.company || unit.client_name, unit.city].filter(Boolean).join(', ');
    }

    function onAwaitingKeydown(e, unitId) {
        if (e.key === 'Enter' || e.key === ' ') {
            e.preventDefault();
            openUnit(unitId);
        }
    }

    function createNew() {
        goto(`${base}/dashboard/repairs/new`);
    }

    // Readiness-grid sort: timestamp the current stage started at (status
    // change → explicit field; falls back through start/create (NOT updated_at: any edit or link write bumps it) so
    // older rows without `status_changed_at` still sort sensibly).
    function stageSince(order) {
        return order.status_changed_at || order.started_at || order.created_at;
    }

    const READINESS_RANK = { pending: 0, received: 1, processing: 2, completed: 3, cancelled: 4 };
    function readinessRank(order) {
        const r = READINESS_RANK[(order.status || '').toLowerCase()];
        return r === undefined ? 2 : r;
    }

    function cmp(a, b) {
        const ra = readinessRank(a);
        const rb = readinessRank(b);
        if (ra !== rb) return ra - rb;
        // Tie: longest in the current stage first (oldest stageSince on top).
        const ta = new Date(stageSince(a) || 0).getTime();
        const tb = new Date(stageSince(b) || 0).getTime();
        return ta - tb;
    }

    $: sorted = [...orders].sort(cmp);

    function stageAgeText(order) {
        const since = stageSince(order);
        if (!since) return '-';
        const ms = Date.now() - new Date(since).getTime();
        const hours = ms / 3_600_000;
        if (hours < 1) return tr('lists.stage_age_now');
        if (hours < 24) return tr('lists.stage_age_hours', { n: Math.floor(hours) });
        return tr('lists.stage_age_days', { n: Math.floor(hours / 24) });
    }

    // Plate tint by time in stage: blue (fresh) -> red at RED_AFTER_DAYS+.
    // Deliberately muted (low saturation / lightness) so it reads as a tint
    // on the dark grey plate, not as a signal colour.
    const RED_AFTER_DAYS = 14;
    function plateStyle(order) {
        const since = stageSince(order);
        const days = since ? (Date.now() - new Date(since).getTime()) / 86_400_000 : 0;
        const k = Math.max(0, Math.min(1, days / RED_AFTER_DAYS));
        const hue = Math.round(215 * (1 - k));       // 215 = blue ... 0 = red
        const sat = Math.round(22 + 22 * k);          // 22% -> 44%
        return `background: hsla(${hue}, ${sat}%, 24%, 0.82);`;
    }

    // Company (requester firm) + city. The backend resolves `company` from the
    // order's support ticket and `city` (order.metadata.city, else ticket city);
    // the requester PERSON and the GER/order number are never shown here.
    function whereText(order) {
        const company = order.company || order.customer_name;
        const city = order.city || (order.metadata && order.metadata.city);
        return [company, city].filter(Boolean).join(', ');
    }

    function onCardKeydown(e, id) {
        if (e.key === 'Enter' || e.key === ' ') {
            e.preventDefault();
            openRepair(id);
        }
    }
</script>

<div class="rma-page">
    <header>
        <h1>{$t('lists.repairs_title')}</h1>
        <div class="actions">
            <button class="action-btn primary" on:click={createNew}>{$t('lists.new_repair')}</button>
        </div>
    </header>

    {#if loading}
        <div class="loading">{$t('lists.repairs_loading')}</div>
    {:else if error}
        <div class="error">{error}</div>
    {:else}
        <div class="table-container">
            {#if orders.length === 0 && awaiting.length === 0}
                <div class="empty-state">{$t('lists.repairs_empty')}</div>
            {:else}
                <div class="cards">
                    {#each awaiting as unit (unit.unit_id)}
                        <button
                            class="card"
                            on:click={() => openUnit(unit.unit_id)}
                            on:keydown={(e) => onAwaitingKeydown(e, unit.unit_id)}
                        >
                            <div class="card-photo">
                                {#if unit.icon_file_id}
                                    <AuthImg class="photo-img" src={iconUrl(unit.icon_file_id)} alt="">
                                        <span slot="fallback" class="photo-glyph">🔧</span>
                                    </AuthImg>
                                {:else}
                                    <span class="photo-glyph">🔧</span>
                                {/if}
                                <span class="status-badge awaiting">
                                    {$t('lists.status_awaiting')}
                                </span>
                            </div>
                            <div class="card-body awaiting-body">
                                <div class="code">{unit.serial || unit.code || '-'}</div>
                                <div class="customer">{awaitingWhere(unit) || '-'}</div>
                                <div class="product"></div>
                                <div class="meta">{receivedAgoText(unit)}</div>
                            </div>
                        </button>
                    {/each}
                    {#each sorted as order}
                        <button
                            class="card"
                            on:click={() => openRepair(leafId(order))}
                            on:keydown={(e) => onCardKeydown(e, leafId(order))}
                        >
                            <div class="card-photo">
                                {#if order.icon_file_id}
                                    <AuthImg class="photo-img" src={iconUrl(order.icon_file_id)} alt="">
                                        <span slot="fallback" class="photo-glyph">🔧</span>
                                    </AuthImg>
                                {:else}
                                    <span class="photo-glyph">🔧</span>
                                {/if}
                                <span class="status-badge {order.status.toLowerCase()}">
                                    {$t('lists.status_' + order.status.toLowerCase())}
                                </span>
                            </div>
                            <div class="card-body" style={plateStyle(order)}>
                                <div class="code">{order.serial_number || order.product_sku || '-'}</div>
                                <div class="customer">{whereText(order) || '-'}</div>
                                <div class="product">{order.product_name || ''}</div>
                                <div class="meta">{stageAgeText(order)}</div>
                            </div>
                        </button>
                    {/each}
                </div>
            {/if}
        </div>
    {/if}
</div>

<style>
    header { display: flex; justify-content: space-between; align-items: center; margin-bottom: 2rem; }
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
        padding: 1rem;
    }

    .cards {
        display: grid;
        grid-template-columns: repeat(auto-fill, minmax(210px, 1fr));
        gap: 1rem;
    }

    .card {
        display: flex;
        flex-direction: column;
        background: #252525;
        border: 1px solid #333;
        border-radius: 8px;
        overflow: hidden;
        cursor: pointer;
        padding: 0;
        text-align: left;
        font: inherit;
        color: inherit;
        transition: border-color 0.1s;
    }
    .card:hover, .card:focus-visible {
        border-color: #4a69bd;
        outline: none;
    }

    .card-photo {
        position: relative;
        width: 100%;
        aspect-ratio: 1 / 1;
        background: #111;
        display: flex;
        align-items: center;
        justify-content: center;
        overflow: hidden;
    }
    .card-photo :global(.photo-img) {
        width: 100%;
        height: 100%;
        object-fit: cover;
    }
    .photo-glyph {
        font-size: 3rem;
        display: flex;
        align-items: center;
        justify-content: center;
    }

    .card-body {
        padding: 0.75rem;
        display: flex;
        flex-direction: column;
        gap: 0.2rem;
    }

    .code { font-family: monospace; font-weight: bold; color: #e8eefc; font-size: 1.05rem; }
    .product {
        font-size: 0.8rem;
        color: #888;
        white-space: nowrap;
        overflow: hidden;
        text-overflow: ellipsis;
    }
    .customer {
        font-weight: 600;
        color: #e0e0e0;
        white-space: nowrap;
        overflow: hidden;
        text-overflow: ellipsis;
    }
    .meta {
        font-size: 0.75rem;
        color: #aaa;
        white-space: nowrap;
        overflow: hidden;
        text-overflow: ellipsis;
    }

    .status-badge {
        position: absolute;
        top: 0.5rem;
        left: 0.5rem;
        padding: 2px 8px;
        border-radius: 12px;
        font-size: 0.7rem;
        font-weight: 600;
        text-transform: uppercase;
    }
    /* Awaiting-triage cards carry a neutral slate plate (no blue->red tint,
       they are not in a repair stage yet) and a neutral badge. */
    .awaiting-body { background: hsla(215, 12%, 26%, 0.82); }
    .status-badge.awaiting   { background: #5a6675; color: #fff; }
    .status-badge.pending    { background: #d35400; color: #fff; }
    .status-badge.received   { background: #f39c12; color: #000; }
    .status-badge.processing { background: #3498db; color: #fff; }
    .status-badge.completed  { background: #27ae60; color: #fff; }
    .status-badge.cancelled  { background: #7f8c8d; color: #fff; }

    .empty-state { padding: 3rem; text-align: center; color: #666; }
</style>
