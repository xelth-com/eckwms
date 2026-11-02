<script>
    // Generic entity object page (ENTITY_PAGES_AND_SCAN_MIRROR §1): a header
    // (icon/photo, title, subtitle, status badge) over the full-width
    // EntityPanel (photos, up/down links, history, scoped chat). Reached via
    // links and the scan tray — intentionally NOT registered in the keep-alive
    // registry.
    import { page } from '$app/stores';
    import { goto, afterNavigate } from '$app/navigation';
    import { base } from '$app/paths';
    import { t } from '$lib/i18n';
    import EntityPanel from '$lib/components/EntityPanel.svelte';
    import AuthImg from '$lib/components/AuthImg.svelte';

    $: kind = $page.params.kind;
    $: id = $page.params.id;

    // Header facts, filled from the EntityPanel's `loaded` event (one fetch
    // shared between the panel and this header).
    let header = null; // { title, subtitle, status, icon_file_id, kind, id }
    function onLoaded(e) {
        header = e.detail;
    }

    const KIND_GLYPH = {
        order: '🔧', receipt: '📥', unit: '📦', place: '📍', item: '🔩', product: '🏷️',
    };
    $: glyph = KIND_GLYPH[kind] || '•';

    // Remember where we came from so "← Back" returns there.
    let backTo = null;
    afterNavigate((nav) => {
        if (nav.from?.url) backTo = nav.from.url.pathname + nav.from.url.search;
    });
    function goBack() {
        if (backTo) goto(backTo);
        else goto(`${base}/dashboard`);
    }
</script>

<div class="obj-page">
    <div class="top">
        <button class="back-btn" on:click={goBack} title={$t('entity.back')}>{$t('entity.back')}</button>
    </div>

    <header class="obj-header">
        <div class="obj-icon">
            {#if header?.icon_file_id}
                <AuthImg class="obj-icon-img" src="{base}/api/files/{header.icon_file_id}" alt="">
                    <span slot="fallback" class="obj-glyph">{glyph}</span>
                </AuthImg>
            {:else}
                <span class="obj-glyph">{glyph}</span>
            {/if}
        </div>
        <div class="obj-titles">
            <h1>{header?.title || `${kind} · ${id}`}</h1>
            {#if header?.subtitle}
                <div class="obj-subtitle">{header.subtitle}</div>
            {/if}
        </div>
        {#if header?.status}
            <span class="status-badge {String(header.status).toLowerCase()}">{header.status}</span>
        {/if}
    </header>

    <EntityPanel {kind} {id} on:loaded={onLoaded} />
</div>

<style>
    .obj-page { max-width: 1100px; margin: 0 auto; padding-bottom: 2rem; }
    .top { margin-bottom: 0.75rem; }
    .back-btn { background: none; border: none; color: #888; cursor: pointer; font-size: 1rem; padding: 0; }
    .back-btn:hover { color: #fff; }

    .obj-header {
        display: flex;
        align-items: center;
        gap: 1rem;
        margin-bottom: 1.5rem;
    }
    .obj-icon {
        width: 64px;
        height: 64px;
        flex: 0 0 auto;
        border-radius: 12px;
        overflow: hidden;
        background: #121212;
        border: 1px solid #333;
        display: flex;
        align-items: center;
        justify-content: center;
    }
    .obj-icon :global(.obj-icon-img) { width: 100%; height: 100%; object-fit: cover; }
    .obj-glyph { font-size: 2rem; }
    .obj-titles { flex: 1 1 auto; min-width: 0; }
    .obj-titles h1 { color: #fff; font-size: 1.6rem; margin: 0; word-break: break-word; }
    .obj-subtitle { color: #999; font-size: 0.95rem; margin-top: 0.2rem; }

    .status-badge {
        padding: 2px 10px;
        border-radius: 12px;
        font-size: 0.75rem;
        font-weight: 600;
        text-transform: uppercase;
        background: #333;
        color: #ddd;
        white-space: nowrap;
    }
    .status-badge.pending    { background: #d35400; color: #fff; }
    .status-badge.received   { background: #f39c12; color: #000; }
    .status-badge.processing { background: #3498db; color: #fff; }
    .status-badge.completed  { background: #27ae60; color: #fff; }
    .status-badge.cancelled  { background: #7f8c8d; color: #fff; }
</style>
