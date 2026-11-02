<script>
    // Entity object panel (ENTITY_PAGES_AND_SCAN_MIRROR §2). Fetches the
    // EntityView for {kind,id} and renders its photos, up/down links, event
    // history and a context-scoped Brain chat. Read-only: observer users see
    // everything, there are no write actions. Degrades gracefully — before the
    // backend lands, missing sections render as empty states rather than break.
    import { createEventDispatcher } from 'svelte';
    import { api } from '$lib/api';
    import { base } from '$app/paths';
    import { t, tr } from '$lib/i18n';
    import { toastStore } from '$lib/stores/toastStore';
    import { openFile } from '$lib/openFile';
    import ChatPanel from '$lib/components/ChatPanel.svelte';
    import AuthImg from '$lib/components/AuthImg.svelte';

    export let kind;
    export let id;

    // Emitted with the fetched EntityView so a host page can render its header
    // (title/subtitle/status/icon) without a second /api/entity request.
    const dispatch = createEventDispatcher();

    let view = null;
    let loading = true;
    let error = null;

    // Lightbox: the currently-opened original photo, or null.
    let lightbox = null; // { file_id, label, image_summary }

    // Kind → glyph, the fallback when a node/entity has no representative photo.
    const KIND_GLYPH = {
        order: '🔧',
        receipt: '📥',
        unit: '📦',
        place: '📍',
        item: '🔩',
        product: '🏷️',
        partner: '🏢',
    };
    function glyphFor(k) {
        return KIND_GLYPH[k] || '•';
    }

    // An `order` node has its own detail page; everything else uses the generic
    // object page.
    function linkFor(node) {
        if (!node) return '#';
        if (node.kind === 'order') return `${base}/dashboard/repairs/${node.id}`;
        return `${base}/dashboard/obj/${node.kind}/${node.id}`;
    }

    function fileUrl(fileId, original = false) {
        return `${base}/api/files/${fileId}${original ? '?original=1' : ''}`;
    }

    function formatTime(dateStr) {
        if (!dateStr) return '';
        try {
            return new Date(dateStr).toLocaleString();
        } catch {
            return String(dateStr);
        }
    }

    async function load() {
        loading = true;
        error = null;
        try {
            view = await api.get(`/api/entity/${kind}/${id}`);
            dispatch('loaded', view);
        } catch (e) {
            console.error('entity load', e);
            error = e.message;
            view = null;
        } finally {
            loading = false;
        }
    }

    // Reload whenever the target entity changes (SPA nav between obj pages
    // reuses this component instance).
    let loadedKey = null;
    $: if (kind && id && `${kind}/${id}` !== loadedKey) {
        loadedKey = `${kind}/${id}`;
        load();
    }

    $: photos = view?.photos ?? [];
    $: up = view?.up ?? [];
    $: down = view?.down ?? [];
    $: events = view?.events ?? [];
    $: fields = view?.fields ?? {};
    $: fieldEntries = Object.entries(fields).filter(([, v]) => v !== null && v !== undefined && v !== '');
    // Clear-tier requester facts (company + city) and the repair documents.
    // The requester PERSON is never part of this payload.
    $: customer = view?.customer ?? null;
    $: documents = view?.documents ?? [];

    // Opening a document goes through the Bearer token (plain href → 401).
    async function openDoc(doc) {
        try {
            await openFile(fileUrl(doc.file_id, true), doc.name || '');
        } catch (e) {
            toastStore.add(tr('entity.open_failed', { error: e.message }), 'error');
        }
    }

    function openLightbox(photo) {
        lightbox = photo;
    }
    function closeLightbox() {
        lightbox = null;
    }
    function onLightboxKeydown(e) {
        if (lightbox && e.key === 'Escape') closeLightbox();
    }
</script>

<svelte:window on:keydown={onLightboxKeydown} />

<div class="entity-panel">
    {#if loading}
        <div class="state">{$t('entity.loading')}</div>
    {:else if error}
        <div class="state error">{$t('entity.load_failed', { error })}</div>
    {/if}

    {#if customer && (customer.company || customer.city)}
        <div class="customer-bar">
            {#if customer.company}<span class="cust-company">{customer.company}</span>{/if}
            {#if customer.city}<span class="cust-city">{customer.city}</span>{/if}
        </div>
    {/if}

    <div class="panel-grid">
        <!-- ── Photos ──────────────────────────────────────────────────── -->
        <section class="card photos-card">
            <h3>{$t('entity.photos')}</h3>
            {#if photos.length === 0}
                <div class="empty">{$t('entity.no_photos')}</div>
            {:else}
                <div class="photo-grid">
                    {#each photos as photo, pi (photo.file_id + '#' + pi)}
                        <div
                            class="photo"
                            class:is-icon={view?.icon_file_id && photo.file_id === view.icon_file_id}
                            role="button"
                            tabindex="0"
                            title={photo.image_summary || photo.label || ''}
                            on:click={() => openLightbox(photo)}
                            on:keydown={(e) => (e.key === 'Enter' || e.key === ' ') && (e.preventDefault(), openLightbox(photo))}
                        >
                            <AuthImg src={fileUrl(photo.file_id)} alt={photo.label || $t('entity.photo_alt')}>
                                <span slot="fallback" class="photo-fallback">🖼️</span>
                            </AuthImg>
                            {#if view?.icon_file_id && photo.file_id === view.icon_file_id}
                                <span class="icon-badge" title={$t('entity.icon_photo')}>★</span>
                            {/if}
                            {#if photo.image_summary}
                                <div class="photo-caption">{photo.image_summary}</div>
                            {/if}
                        </div>
                    {/each}
                </div>
            {/if}
        </section>

        <!-- ── Facts ───────────────────────────────────────────────────── -->
        {#if fieldEntries.length > 0}
            <section class="card facts-card">
                <h3>{$t('entity.facts')}</h3>
                <dl class="facts">
                    {#each fieldEntries as [key, value]}
                        <dt>{key}</dt>
                        <dd>{typeof value === 'object' ? JSON.stringify(value) : value}</dd>
                    {/each}
                </dl>
            </section>
        {/if}

        <!-- ── Documents (repair protocols / PDFs) ─────────────────────── -->
        {#if documents.length > 0}
            <section class="card docs-card">
                <h3>{$t('entity.documents')}</h3>
                <div class="doc-list">
                    {#each documents as doc (doc.file_id)}
                        <div class="doc-row">
                            <span class="doc-icon" aria-hidden="true">📄</span>
                            <span class="doc-text">
                                <span class="doc-name">{doc.name || doc.file_id}</span>
                                <span class="doc-tags">
                                    <span class="doc-kind kind-{doc.kind}">{$t('entity.kind_' + doc.kind)}</span>
                                    <span class="doc-source">{doc.source}</span>
                                </span>
                            </span>
                            <button type="button" class="doc-open" on:click={() => openDoc(doc)}>
                                {$t('entity.open_file')}
                            </button>
                        </div>
                    {/each}
                </div>
            </section>
        {/if}

        <!-- ── Linked up ───────────────────────────────────────────────── -->
        <section class="card">
            <h3>{$t('entity.linked_up')}</h3>
            {#if up.length === 0}
                <div class="empty">{$t('entity.no_links')}</div>
            {:else}
                <div class="link-list">
                    {#each up as node, ui (node.kind + '/' + node.id + '#' + ui)}
                        <a class="link-card" href={linkFor(node)}>
                            {#if node.icon_file_id}
                                <AuthImg class="link-icon" src={fileUrl(node.icon_file_id)} alt="">
                                    <span slot="fallback" class="link-glyph">{glyphFor(node.kind)}</span>
                                </AuthImg>
                            {:else}
                                <span class="link-glyph">{glyphFor(node.kind)}</span>
                            {/if}
                            <span class="link-text">
                                <span class="link-title">{node.title || node.id}</span>
                                {#if node.subtitle}<span class="link-sub">{node.subtitle}</span>{/if}
                            </span>
                        </a>
                    {/each}
                </div>
            {/if}
        </section>

        <!-- ── Linked down ─────────────────────────────────────────────── -->
        <section class="card">
            <h3>{$t('entity.linked_down')}</h3>
            {#if down.length === 0}
                <div class="empty">{$t('entity.no_links')}</div>
            {:else}
                <div class="link-list">
                    {#each down as node, di (node.kind + '/' + node.id + '#' + di)}
                        <a class="link-card" href={linkFor(node)}>
                            {#if node.icon_file_id}
                                <AuthImg class="link-icon" src={fileUrl(node.icon_file_id)} alt="">
                                    <span slot="fallback" class="link-glyph">{glyphFor(node.kind)}</span>
                                </AuthImg>
                            {:else}
                                <span class="link-glyph">{glyphFor(node.kind)}</span>
                            {/if}
                            <span class="link-text">
                                <span class="link-title">{node.title || node.id}</span>
                                {#if node.subtitle}<span class="link-sub">{node.subtitle}</span>{/if}
                            </span>
                        </a>
                    {/each}
                </div>
            {/if}
        </section>

        <!-- ── History ─────────────────────────────────────────────────── -->
        <section class="card history-card">
            <h3>{$t('entity.history')}</h3>
            {#if events.length === 0}
                <div class="empty">{$t('entity.no_history')}</div>
            {:else}
                <ul class="timeline">
                    {#each events as ev, i (i)}
                        <li class="event">
                            <div class="event-head">
                                <span class="event-kind kind-{ev.kind}">{ev.kind}</span>
                                <span class="event-time">{formatTime(ev.at)}</span>
                            </div>
                            <div class="event-text">{ev.text || ''}</div>
                            <div class="event-meta">
                                {#if ev.actor}<span class="event-actor">{ev.actor}</span>{/if}
                                {#if ev.ref}
                                    <a class="event-ref" href={linkFor(ev.ref)}>
                                        {glyphFor(ev.ref.kind)} {ev.ref.title || ev.ref.id}
                                    </a>
                                {/if}
                            </div>
                        </li>
                    {/each}
                </ul>
            {/if}
        </section>

        <!-- ── Chat scoped to this entity ──────────────────────────────── -->
        <section class="card chat-card">
            <h3>{$t('entity.chat')}</h3>
            <div class="chat-wrap">
                <ChatPanel context={{ kind, id }} compact={true} />
            </div>
        </section>
    </div>
</div>

{#if lightbox}
    <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
    <div class="lightbox" role="dialog" aria-modal="true" tabindex="-1" on:click={closeLightbox}>
        <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_noninteractive_element_interactions -->
        <div class="lightbox-inner" role="document" on:click|stopPropagation>
            <button class="lightbox-close" on:click={closeLightbox} aria-label={$t('entity.close')}>×</button>
            <AuthImg class="lightbox-img" src={fileUrl(lightbox.file_id, true)} alt={lightbox.label || $t('entity.photo_alt')}>
                <span slot="fallback" class="lightbox-fallback">🖼️</span>
            </AuthImg>
            {#if lightbox.image_summary}
                <div class="lightbox-caption">{lightbox.image_summary}</div>
            {/if}
        </div>
    </div>
{/if}

<style>
    .entity-panel { width: 100%; }
    .state { padding: 1rem; color: #aaa; text-align: center; }
    .state.error { color: #ff6b6b; }

    .panel-grid {
        display: grid;
        grid-template-columns: repeat(2, minmax(0, 1fr));
        gap: 1.25rem;
        align-items: start;
    }
    .card {
        background: #1e1e1e;
        border: 1px solid #333;
        border-radius: 8px;
        padding: 1.25rem;
        min-width: 0;
    }
    .card h3 {
        margin: 0 0 1rem;
        color: #ccc;
        font-size: 1rem;
        border-bottom: 1px solid #333;
        padding-bottom: 0.5rem;
    }
    .photos-card, .history-card, .chat-card, .facts-card, .docs-card { grid-column: 1 / -1; }
    .empty { color: #666; font-style: italic; font-size: 0.9rem; padding: 0.5rem 0; }

    /* ── Customer bar (company · city, clear-tier) ─────────────────────────── */
    .customer-bar {
        display: flex;
        align-items: baseline;
        gap: 0.75rem;
        margin-bottom: 1rem;
        padding: 0.6rem 0.9rem;
        background: #1a2540;
        border: 1px solid #27406e;
        border-radius: 8px;
    }
    .cust-company { color: #e8eefc; font-weight: 700; font-size: 1.05rem; }
    .cust-city { color: #a3bffa; font-size: 0.9rem; }

    /* ── Documents ─────────────────────────────────────────────────────────── */
    .doc-list { display: flex; flex-direction: column; gap: 0.6rem; }
    .doc-row {
        display: flex;
        align-items: center;
        gap: 0.75rem;
        background: #1a1a1a;
        border: 1px solid #2a2a2a;
        border-radius: 6px;
        padding: 0.6rem 0.8rem;
    }
    .doc-icon { font-size: 1.4rem; flex: 0 0 auto; }
    .doc-text { display: flex; flex-direction: column; min-width: 0; flex: 1 1 auto; gap: 0.2rem; }
    .doc-name { color: #e0e0e0; font-weight: 600; font-size: 0.9rem; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .doc-tags { display: flex; gap: 0.5rem; align-items: center; }
    .doc-kind {
        font-size: 0.68rem;
        text-transform: uppercase;
        letter-spacing: 0.4px;
        font-weight: 700;
        color: #a3bffa;
        background: #1a2540;
        border: 1px solid #27406e;
        border-radius: 4px;
        padding: 0.05rem 0.4rem;
    }
    .doc-kind.kind-repair_order { color: #86efac; background: #11291a; border-color: #1f5133; }
    .doc-kind.kind-dsgvo { color: #fcd34d; background: #2a2208; border-color: #5a4a12; }
    .doc-source { color: #777; font-size: 0.72rem; text-transform: uppercase; letter-spacing: 0.3px; }
    .doc-open {
        flex: 0 0 auto;
        background: #4a69bd;
        color: #fff;
        border: none;
        border-radius: 4px;
        padding: 0.4rem 0.9rem;
        font-weight: 600;
        font-size: 0.85rem;
        cursor: pointer;
    }
    .doc-open:hover { background: #3b5bdb; }

    /* ── Photos ──────────────────────────────────────────────────────────── */
    .photo-grid {
        display: grid;
        grid-template-columns: repeat(auto-fill, minmax(140px, 1fr));
        gap: 0.9rem;
    }
    .photo {
        margin: 0;
        position: relative;
        background: #121212;
        border: 1px solid #2a2a2a;
        border-radius: 8px;
        overflow: hidden;
        cursor: pointer;
        transition: border-color 0.15s;
    }
    .photo:hover, .photo:focus-visible { border-color: #4a69bd; outline: none; }
    .photo.is-icon { border-color: #f59e0b; }
    .photo :global(img) { display: block; width: 100%; height: 120px; object-fit: cover; }
    .photo-fallback {
        display: flex;
        align-items: center;
        justify-content: center;
        height: 120px;
        font-size: 2rem;
        color: #555;
        background: #121212;
    }
    .photo-caption {
        padding: 0.4rem 0.5rem;
        font-size: 0.75rem;
        color: #aaa;
        line-height: 1.3;
        display: -webkit-box;
        -webkit-line-clamp: 2;
        line-clamp: 2;
        -webkit-box-orient: vertical;
        overflow: hidden;
    }
    .icon-badge {
        position: absolute;
        top: 0.3rem;
        left: 0.3rem;
        background: rgba(245, 158, 11, 0.9);
        color: #1a1a1a;
        font-size: 0.75rem;
        width: 1.2rem;
        height: 1.2rem;
        border-radius: 50%;
        display: flex;
        align-items: center;
        justify-content: center;
        font-weight: 700;
    }

    /* ── Facts ───────────────────────────────────────────────────────────── */
    .facts {
        display: grid;
        grid-template-columns: max-content 1fr;
        gap: 0.4rem 1rem;
        margin: 0;
    }
    .facts dt { color: #888; font-size: 0.85rem; }
    .facts dd { margin: 0; color: #e0e0e0; font-size: 0.9rem; word-break: break-word; }

    /* ── Linked up / down ─────────────────────────────────────────────────── */
    .link-list { display: flex; flex-direction: column; gap: 0.6rem; }
    .link-card {
        display: flex;
        align-items: center;
        gap: 0.75rem;
        background: #1a1a1a;
        border: 1px solid #2a2a2a;
        border-radius: 6px;
        padding: 0.6rem 0.8rem;
        text-decoration: none;
        transition: border-color 0.15s, background 0.15s;
    }
    .link-card:hover { border-color: #4a69bd; background: #222; }
    .link-card :global(.link-icon) {
        width: 40px;
        height: 40px;
        border-radius: 50%;
        object-fit: cover;
        flex: 0 0 auto;
        background: #121212;
    }
    .link-glyph {
        width: 40px;
        height: 40px;
        border-radius: 50%;
        background: #121212;
        border: 1px solid #333;
        display: flex;
        align-items: center;
        justify-content: center;
        font-size: 1.2rem;
        flex: 0 0 auto;
    }
    .link-text { display: flex; flex-direction: column; min-width: 0; }
    .link-title { color: #e0e0e0; font-weight: 600; font-size: 0.9rem; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .link-sub { color: #888; font-size: 0.8rem; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

    /* ── History ───────────────────────────────────────────────────────────── */
    .timeline { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 0.75rem; }
    .event {
        border-left: 2px solid #333;
        padding: 0 0 0 0.9rem;
        position: relative;
    }
    .event-head { display: flex; align-items: center; gap: 0.6rem; margin-bottom: 0.2rem; }
    .event-kind {
        font-size: 0.7rem;
        text-transform: uppercase;
        letter-spacing: 0.4px;
        font-weight: 700;
        color: #a3bffa;
        background: #1a2540;
        border: 1px solid #27406e;
        border-radius: 4px;
        padding: 0.05rem 0.4rem;
    }
    .event-kind.kind-scan { color: #86efac; background: #11291a; border-color: #1f5133; }
    .event-kind.kind-move { color: #fcd34d; background: #2a2208; border-color: #5a4a12; }
    .event-kind.kind-photo { color: #c4b5fd; background: #211a40; border-color: #3b2f6e; }
    .event-time { color: #666; font-size: 0.78rem; }
    .event-text { color: #ddd; font-size: 0.9rem; line-height: 1.4; }
    .event-meta { display: flex; gap: 0.75rem; align-items: center; margin-top: 0.2rem; }
    .event-actor { color: #888; font-size: 0.78rem; }
    .event-ref { color: #6c9bd8; font-size: 0.8rem; text-decoration: none; }
    .event-ref:hover { text-decoration: underline; }

    /* ── Chat ──────────────────────────────────────────────────────────────── */
    .chat-wrap { height: 420px; }

    /* ── Lightbox ──────────────────────────────────────────────────────────── */
    .lightbox {
        position: fixed;
        inset: 0;
        background: rgba(0, 0, 0, 0.85);
        z-index: 9999;
        display: flex;
        align-items: center;
        justify-content: center;
        padding: 2rem;
    }
    .lightbox-inner {
        position: relative;
        max-width: 92vw;
        max-height: 90vh;
        display: flex;
        flex-direction: column;
        gap: 0.5rem;
    }
    .lightbox-inner :global(.lightbox-img) {
        max-width: 92vw;
        max-height: 80vh;
        object-fit: contain;
        border-radius: 6px;
    }
    .lightbox-fallback { font-size: 4rem; color: #555; }
    .lightbox-caption { color: #ddd; font-size: 0.9rem; text-align: center; }
    .lightbox-close {
        position: absolute;
        top: -0.5rem;
        right: -0.5rem;
        background: #1a1a1a;
        border: 1px solid #444;
        color: #fff;
        width: 2rem;
        height: 2rem;
        border-radius: 50%;
        font-size: 1.3rem;
        line-height: 1;
        cursor: pointer;
    }
    .lightbox-close:hover { background: #333; }

    @media (max-width: 700px) {
        .panel-grid { grid-template-columns: minmax(0, 1fr); }
    }
</style>
