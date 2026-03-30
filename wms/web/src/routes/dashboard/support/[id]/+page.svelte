<script>
    import { page } from '$app/stores';
    import { onMount } from 'svelte';
    import { api } from '$lib/api';
    import { goto } from '$app/navigation';
    import { base } from '$app/paths';
    import { toastStore } from '$lib/stores/toastStore.js';

    const ticketId = $page.params.id;

    let threads = [];
    let loading = true;
    let error = null;

    let allTickets = [];
    let relatedTickets = [];

    // Track which threads are expanded (by documentId)
    let expandedThreads = new Set();

    // attachment arrays keyed by document UUID
    let attachments = {};

    onMount(async () => {
        await loadThreads();
        loadSimilar();
        // Load all tickets asynchronously for the "Related Tickets" feature
        api.get('/api/support/tickets').then(res => {
            allTickets = res || [];
            computeRelatedTickets();
        }).catch(e => console.warn('Failed to load related tickets', e));
    });

    async function loadThreads() {
        loading = true;
        error = null;
        try {
            threads = await api.get(`/api/support/tickets/${ticketId}/threads`);
            // Load attachments for every thread in parallel
            await Promise.all(threads.map(t => loadAttachments(t.documentId)));
            computeRelatedTickets();
        } catch (e) {
            console.error(e);
            error = e.message;
            toastStore.add('Failed to load threads', 'error');
        } finally {
            loading = false;
        }
    }

    async function loadAttachments(docId) {
        if (!docId) return;
        try {
            const res = await api.get(`/api/attachments/document/${docId}`);
            attachments[docId] = res || [];
        } catch {
            attachments[docId] = [];
        }
        // Trigger reactivity
        attachments = attachments;
    }

    // Reactively compute contact info
    $: ticketNumber = threads[0]?.payload?.ticket?.ticketNumber ?? ticketId;
    $: subject = threads[0]?.payload?.ticket?.subject ?? ticketId;
    $: ticketStatus = threads[0]?.payload?.ticket?.status ?? '';

    $: contact = threads[0]?.payload?.ticket?.contact || {};
    $: customer = contact.fullName || [contact.firstName, contact.lastName].filter(Boolean).join(' ') || threads[0]?.payload?.from || '';
    $: customerEmail = contact.email || '';
    $: customerPhone = contact.phone || '';

    function findVal(meta, keys) {
        if (!meta) return '';
        const cfs = meta.customFields || {};
        for (const [k, v] of Object.entries(cfs)) {
            if (keys.some(kw => k.toLowerCase().includes(kw)) && v) return String(v);
        }
        for (const [k, v] of Object.entries(meta)) {
            if (keys.some(kw => k.toLowerCase().includes(kw)) && typeof v === 'string' && v) return v;
        }
        return '';
    }

    $: meta = threads[0]?.payload?.ticket || {};
    $: company = findVal(meta, ["company", "einrichtung"]);
    $: address = findVal(meta, ["address", "adresse"]);
    $: deviceModel = findVal(meta, ["inbody model", "inbodymodel"]);
    $: serialNumber = findVal(meta, ["serial", "seriennummer"]);
    $: manufacturingDate = findVal(meta, ["herstellungsdatum", "manufacturing date", "manufacturing"]);

    function getWarrantyStatus(dateStr) {
        if (!dateStr) return null;
        const mfgDate = new Date(dateStr);
        if (isNaN(mfgDate)) return null;

        const ageYears = (Date.now() - mfgDate.getTime()) / (1000 * 60 * 60 * 24 * 365.25);

        if (ageYears < 2.0) return { text: "Warranty Active", class: "w-ok" };
        if (ageYears < 2.3) return { text: "Likely Warranty (Check Purchase Date)", class: "w-check" };
        if (ageYears < 2.5) return { text: "Possible Goodwill (Kulanz)", class: "w-goodwill" };
        return { text: "Out of Warranty", class: "w-expired" };
    }

    function computeRelatedTickets() {
        if (allTickets.length === 0 || threads.length === 0) return;

        const currentEmail = customerEmail.toLowerCase();
        const currentPhone = customerPhone.replace(/\D/g, '');
        const currentSerial = serialNumber.toLowerCase().trim();
        const currentCompany = company.toLowerCase().trim();

        // Determine private domain
        const publicDomains = ['gmail.com', 'gmx.de', 'web.de', 't-online.de', 'yahoo.com', 'hotmail.com', 'outlook.com', 'icloud.com', 'mail.ru', 'freenet.de', 'me.com', 'mac.com'];
        let domain = '';
        if (currentEmail.includes('@')) {
            const d = currentEmail.split('@')[1];
            if (!publicDomains.includes(d)) domain = d;
        }

        relatedTickets = allTickets.filter(t => {
            if (t.ticket_id === ticketId) return false;

            const otherEmail = (t.email || '').toLowerCase();
            const otherPhone = (t.phone || '').replace(/\D/g, '');
            const otherSerial = (t.serial_number || '').toLowerCase().trim();
            const otherCompany = (t.company || '').toLowerCase().trim();

            if (currentEmail && otherEmail === currentEmail) return true;
            if (currentPhone && otherPhone === currentPhone) return true;
            if (domain && otherEmail.includes(`@${domain}`)) return true;
            if (currentSerial && otherSerial === currentSerial) return true;
            if (currentCompany && currentCompany.length > 3 && otherCompany === currentCompany) return true;

            return false;
        });
    }

    function formatDate(str) {
        if (!str) return '-';
        try {
            return new Date(str).toLocaleString('de-DE', {
                day: '2-digit', month: '2-digit', year: 'numeric',
                hour: '2-digit', minute: '2-digit'
            });
        } catch { return str; }
    }

    function statusClass(status) {
        const s = (status || '').toLowerCase();
        if (s === 'open') return 'open';
        if (s === 'closed') return 'closed';
        if (s.includes('pending agent')) return 'urgent';
        if (s.includes('research')) return 'research';
        if (s === 'onhold' || s === 'on hold') return 'onhold';
        return 'other';
    }

    function directionLabel(dir) {
        if (dir === 'in') return { label: 'Inbound', cls: 'inbound' };
        if (dir === 'out') return { label: 'Outbound', cls: 'outbound' };
        return { label: dir || '?', cls: 'other' };
    }

    function isImage(mimeType) {
        return (mimeType || '').startsWith('image/');
    }

    function fileIcon(mimeType) {
        const m = mimeType || '';
        if (m.includes('pdf')) return '📄';
        if (m.includes('excel') || m.includes('spreadsheet') || m.includes('xlsx')) return '📊';
        if (m.includes('word') || m.includes('doc')) return '📝';
        if (m.startsWith('image/')) return '🖼️';
        return '📎';
    }

    // ── AI Semantic Matches ─────────────────────────────────────────────────
    let similarEntities = [];
    let similarLoading = false;

    async function loadSimilar() {
        similarLoading = true;
        try {
            similarEntities = await api.get(`/api/support/tickets/${ticketId}/similar`);
        } catch (e) {
            console.warn('Failed to load similar entities', e);
            similarEntities = [];
        } finally {
            similarLoading = false;
        }
    }

    // ── AI Summary ────────────────────────────────────────────────────────────
    let summary = '';
    let isSummarizing = false;
    let summaryError = '';

    async function generateSummary() {
        isSummarizing = true;
        summaryError = '';
        try {
            const res = await api.post(`/api/support/tickets/${ticketId}/summary`, {});
            summary = res.summary ?? '';
            if (!summary) summaryError = 'AI returned an empty response.';
        } catch (e) {
            summaryError = e.message;
            toastStore.add('AI summary failed: ' + e.message, 'error');
        } finally {
            isSummarizing = false;
        }
    }

    // ── Create RMA ────────────────────────────────────────────────────────────
    function createRMA() {
        const params = new URLSearchParams({
            ticketId,
            name:  customer || '',
            email: customerEmail,
            issue: summary || subject || '',
            serial: serialNumber || '',
            model: deviceModel || '',
        });
        goto(`${base}/dashboard/rma/new?${params}`);
    }

    function createRepair() {
        const params = new URLSearchParams({
            ticketId,
            name:  customer || '',
            email: customerEmail,
            issue: summary || subject || '',
            serial: serialNumber || '',
            model: deviceModel || '',
        });
        goto(`${base}/dashboard/repairs/new?${params}`);
    }

    async function copyForAI() {
        if (!threads || threads.length === 0) {
            toastStore.add('No threads to copy', 'warning');
            return;
        }

        const systemPrompt = "You are a technical support assistant. Summarize the following customer support email thread. Extract the core hardware or software problem, any troubleshooting steps already attempted, and the current status. Be concise and professional. Format the result in 2-3 short paragraphs.\n\n---\n\n";

        const parts = threads.map(t => {
            const dir = t.payload?.direction || '?';
            const from = t.payload?.from || '?';
            const time = t.payload?.createdTime || '?';

            const rawContent = t.payload?.content || t.payload?.summary || '';
            const tempDiv = document.createElement("div");
            tempDiv.innerHTML = rawContent;
            const cleanText = (tempDiv.textContent || tempDiv.innerText || '').replace(/\s+/g, ' ').trim();

            if (!cleanText) return null;
            return `[${dir.toUpperCase()} | From: ${from} | ${time}]\n${cleanText}`;
        }).filter(Boolean);

        if (parts.length === 0) {
            toastStore.add('No readable text found in threads', 'warning');
            return;
        }

        const fullText = systemPrompt + parts.join("\n\n---\n\n");

        try {
            await navigator.clipboard.writeText(fullText);
            toastStore.add('Copied! Paste it into ChatGPT, Claude, or Gemini.', 'success', 4000);
        } catch (err) {
            toastStore.add('Failed to copy: ' + err.message, 'error');
        }
    }
</script>

<div class="detail-page">
    <div class="back-link">
        <button class="back-btn" on:click={() => goto(`${base}/dashboard/support`)}>
            ← Back to tickets
        </button>
    </div>

    {#if loading}
        <div class="loading">Loading threads...</div>
    {:else if error}
        <div class="error-box">Failed to load: {error}</div>
    {:else}
        <header class="ticket-header">
            <div class="ticket-meta">
                <span class="ticket-id-badge">#{ticketNumber}</span>
                {#if ticketStatus}
                    <span class="status-chip {statusClass(ticketStatus)}">{ticketStatus}</span>
                {/if}
                <div class="header-actions">
                    <button class="copy-ai-btn" on:click={copyForAI} disabled={threads.length === 0} title="Copy cleaned text & prompt to clipboard">
                        Copy for AI
                    </button>
                    <button class="ai-btn" on:click={generateSummary} disabled={isSummarizing || threads.length === 0}>
                        {#if isSummarizing}
                            <span class="spinner">...</span> Summarizing
                        {:else}
                            Summarize with AI
                        {/if}
                    </button>
                    <button class="rma-btn" on:click={createRMA} disabled={threads.length === 0}>
                        Create RMA
                    </button>
                    <button class="repair-btn" on:click={createRepair} disabled={threads.length === 0}>
                        Create Repair
                    </button>
                </div>
            </div>
            <h1 class="ticket-subject">{subject}</h1>
            <div class="ticket-info-grid">
                <div class="ticket-customer-box">
                    <div class="box-icon">👤</div>
                    <div class="box-details">
                        <div class="box-title">{customer || 'Unknown Customer'}</div>
                        <div class="box-sub">
                            {#if company}<div class="c-item">🏢 {company}</div>{/if}
                            {#if customerEmail}<div class="c-item">✉️ {customerEmail}</div>{/if}
                            {#if customerPhone}<div class="c-item">📞 {customerPhone}</div>{/if}
                            {#if address}<div class="c-item">📍 {address}</div>{/if}
                        </div>
                    </div>
                </div>
                {#if deviceModel || serialNumber || manufacturingDate}
                    <div class="ticket-device-box">
                        <div class="box-icon">💻</div>
                        <div class="box-details">
                            <div class="box-title">{deviceModel || 'Unknown Device'}</div>
                            <div class="box-sub">
                                {#if serialNumber}<div class="c-item mono">SN: {serialNumber}</div>{/if}
                                {#if manufacturingDate}
                                    <div class="c-item">Mfg: {new Date(manufacturingDate).toLocaleDateString('de-DE')}</div>
                                    {@const wStatus = getWarrantyStatus(manufacturingDate)}
                                    {#if wStatus}
                                        <div class="warranty-badge {wStatus.class}">{wStatus.text}</div>
                                    {/if}
                                {/if}
                            </div>
                        </div>
                    </div>
                {/if}
            </div>
        </header>

        {#if relatedTickets.length > 0}
            <div class="related-tickets-banner">
                <div class="related-content">
                    <strong>Found {relatedTickets.length} related ticket{relatedTickets.length > 1 ? 's' : ''}</strong> from the same customer or domain:
                    <div class="related-links">
                        {#each relatedTickets as rt}
                            <a href="{base}/dashboard/support/{rt.ticket_id}" class="related-link">
                                #{rt.ticket_number || rt.ticket_id.substring(0,8)} ({rt.status})
                            </a>
                        {/each}
                    </div>
                </div>
            </div>
        {/if}

        {#if similarLoading || similarEntities.length > 0}
            <div class="semantic-matches-panel">
                <div class="semantic-title">AI Semantic Matches</div>
                {#if similarLoading}
                    <div class="semantic-loading">Searching for similar entities...</div>
                {:else}
                    <div class="semantic-list">
                        {#each similarEntities as entity}
                            <a
                                class="semantic-item"
                                href="{base}/dashboard/{entity.entity_type === 'ticket' ? 'support' : 'repairs'}/{entity.entity_id}"
                            >
                                <span class="sem-type-badge" class:sem-ticket={entity.entity_type === 'ticket'} class:sem-repair={entity.entity_type === 'repair'}>
                                    {entity.entity_type}
                                </span>
                                <span class="sem-label">{entity.label}</span>
                                <span class="sem-score">{Math.round(entity.similarity * 100)}%</span>
                            </a>
                        {/each}
                    </div>
                {/if}
            </div>
        {/if}

        {#if summary || isSummarizing || summaryError}
            <div class="summary-panel">
                <div class="summary-title">AI Summary</div>
                {#if isSummarizing}
                    <div class="summary-loading">Generating summary...</div>
                {:else if summaryError}
                    <div class="summary-error">{summaryError}</div>
                {:else}
                    <div class="summary-text">{summary}</div>
                    <div class="summary-actions">
                        <button class="use-as-issue-btn" on:click={createRMA}>-> Use for new RMA</button>
                        <button class="use-as-issue-btn" on:click={createRepair}>-> Use for new Repair</button>
                    </div>
                {/if}
            </div>
        {/if}

        {#if threads.length === 0}
            <div class="empty-state">No threads found for this ticket.</div>
        {:else}
            <div class="thread-list">
                {#each threads as thread (thread.documentId)}
                    {@const dir = directionLabel(thread.payload?.direction)}
                    {@const isExpanded = expandedThreads.has(thread.documentId)}
                    <div class="thread-card {dir.cls}" class:expanded={isExpanded}>
                        <!-- svelte-ignore a11y-click-events-have-key-events -->
                        <div class="thread-header" on:click={() => {
                            if (expandedThreads.has(thread.documentId)) {
                                expandedThreads.delete(thread.documentId);
                            } else {
                                expandedThreads.add(thread.documentId);
                            }
                            expandedThreads = expandedThreads;
                        }}>
                            <span class="expand-arrow">{isExpanded ? '▼' : '▶'}</span>
                            <span class="dir-badge {dir.cls}">{dir.label}</span>
                            <span class="thread-from">{thread.payload?.from ?? ''}</span>
                            <span class="thread-date">{formatDate(thread.payload?.createdTime)}</span>
                        </div>

                        {#if thread.payload?.content}
                            <!-- svelte-ignore a11y-click-events-have-key-events -->
                            <div class="thread-body-wrapper" class:collapsed={!isExpanded}
                                on:click={() => { if (!isExpanded) { expandedThreads.add(thread.documentId); expandedThreads = expandedThreads; } }}>
                                <div class="thread-html-body">
                                    {@html thread.payload.content}
                                </div>
                                {#if !isExpanded}
                                    <div class="thread-fade"></div>
                                {/if}
                            </div>
                        {:else}
                            <div class="thread-empty">(no content)</div>
                        {/if}

                        {#if isExpanded}
                        {#if attachments[thread.documentId]?.length}
                            <div class="attachment-list">
                                {#each attachments[thread.documentId] as att}
                                    <a
                                        class="attachment-item"
                                        href="{base}/api/files/{att.file_id}"
                                        target="_blank"
                                        rel="noopener noreferrer"
                                        title={att.file_id}
                                    >
                                        {#if isImage(att.mime_type)}
                                            <img
                                                class="att-thumb"
                                                src="{base}/api/files/{att.file_id}"
                                                alt="attachment"
                                            />
                                        {:else}
                                            <span class="att-icon">{fileIcon(att.mime_type)}</span>
                                        {/if}
                                        <span class="att-label">{att.mime_type}</span>
                                    </a>
                                {/each}
                            </div>
                        {/if}
                        {/if}
                    </div>
                {/each}
            </div>
        {/if}
    {/if}
</div>

<style>
    .detail-page { padding: 0; }

    .back-btn {
        background: none;
        border: none;
        color: #4a69bd;
        font-size: 0.9rem;
        cursor: pointer;
        padding: 0 0 1rem 0;
        font-weight: 600;
        transition: color 0.2s;
    }
    .back-btn:hover { color: #93c5fd; }

    .loading { color: #aaa; text-align: center; padding: 3rem; }
    .error-box {
        padding: 2rem;
        color: #ff6b6b;
        background: #1e1e1e;
        border: 1px solid #ff6b6b;
        border-radius: 8px;
    }
    .empty-state { color: #666; padding: 2rem; text-align: center; }

    /* Ticket header */
    .ticket-header {
        background: #1e1e1e;
        border: 1px solid #333;
        border-radius: 10px;
        padding: 1.25rem 1.5rem;
        margin-bottom: 1.5rem;
    }
    .ticket-meta { display: flex; align-items: center; gap: 0.75rem; margin-bottom: 0.5rem; }
    .ticket-id-badge {
        font-family: monospace;
        font-size: 0.85rem;
        font-weight: bold;
        color: #6bc5f0;
        background: #1a2a3a;
        border: 1px solid #4a69bd;
        border-radius: 4px;
        padding: 0.15rem 0.5rem;
    }
    .status-chip {
        font-size: 0.75rem;
        font-weight: 600;
        color: #aaa;
        background: #2a2a2a;
        border: 1px solid #444;
        border-radius: 10px;
        padding: 0.15rem 0.6rem;
        text-transform: capitalize;
    }
    .status-chip.urgent { background: #4a1010; color: #ff6b6b; border-color: #dc3545; }
    .status-chip.research { background: #1a2a4a; color: #a3bffa; border-color: #4a69bd; }
    .ticket-subject { font-size: 1.4rem; color: #fff; margin: 0 0 1rem 0; line-height: 1.3; }

    .ticket-info-grid { display: flex; gap: 1rem; flex-wrap: wrap; }
    .ticket-customer-box, .ticket-device-box {
        flex: 1;
        min-width: 250px;
        display: flex;
        align-items: flex-start;
        gap: 0.75rem;
        background: #252525;
        padding: 0.75rem 1rem;
        border-radius: 8px;
        border: 1px solid #2a2a2a;
    }
    .ticket-device-box { border-color: #4a69bd; background: rgba(74, 105, 189, 0.05); }
    .box-icon { font-size: 1.5rem; margin-top: 0.2rem; }
    .box-details { display: flex; flex-direction: column; gap: 0.3rem; }
    .box-title { font-weight: 600; color: #e0e0e0; font-size: 0.95rem; }
    .ticket-device-box .box-title { color: #a3bffa; }
    .box-sub { display: flex; flex-direction: column; gap: 0.25rem; font-size: 0.8rem; color: #888; align-items: flex-start; }
    .c-item { display: inline-flex; align-items: center; }

    .warranty-badge { font-size: 0.7rem; padding: 0.2rem 0.5rem; border-radius: 4px; display: inline-block; font-weight: 600; margin-top: 2px; }
    .warranty-badge.w-ok { background: rgba(34, 197, 94, 0.15); color: #4ade80; border: 1px solid rgba(34, 197, 94, 0.3); }
    .warranty-badge.w-check { background: rgba(251, 191, 36, 0.15); color: #fbbf24; border: 1px solid rgba(251, 191, 36, 0.3); }
    .warranty-badge.w-goodwill { background: rgba(249, 115, 22, 0.15); color: #fb923c; border: 1px solid rgba(249, 115, 22, 0.3); }
    .warranty-badge.w-expired { background: rgba(156, 163, 175, 0.1); color: #9ca3af; border: 1px solid rgba(156, 163, 175, 0.3); }

    /* Related Tickets Banner */
    .related-tickets-banner {
        display: flex;
        align-items: flex-start;
        gap: 1rem;
        background: #3a2a0a;
        border: 1px solid #f59e0b;
        border-radius: 8px;
        padding: 1rem;
        margin-bottom: 1.5rem;
    }
    .related-content { color: #fef3c7; font-size: 0.9rem; line-height: 1.4; }
    .related-links { display: flex; flex-wrap: wrap; gap: 0.5rem; margin-top: 0.5rem; }
    .related-link {
        background: #b45309;
        color: #fff;
        text-decoration: none;
        padding: 0.2rem 0.6rem;
        border-radius: 4px;
        font-family: monospace;
        font-size: 0.8rem;
        font-weight: bold;
        transition: background 0.2s;
    }
    .related-link:hover { background: #d97706; }

    /* Thread list */
    .thread-list { display: flex; flex-direction: column; gap: 1rem; }

    .thread-card {
        background: #1e1e1e;
        border: 1px solid #333;
        border-radius: 10px;
        overflow: hidden;
    }
    .thread-card.inbound  { border-left: 3px solid #4a69bd; }
    .thread-card.outbound { border-left: 3px solid #22c55e; }

    .thread-header {
        display: flex;
        align-items: center;
        gap: 0.75rem;
        padding: 0.75rem 1rem;
        background: #252525;
        border-bottom: 1px solid #2a2a2a;
        flex-wrap: wrap;
        cursor: pointer;
        user-select: none;
        transition: background 0.15s;
    }
    .thread-header:hover { background: #2a2a2a; }
    .expand-arrow {
        font-size: 0.7rem;
        color: #666;
        flex-shrink: 0;
        width: 1rem;
        text-align: center;
    }
    .thread-card.expanded .expand-arrow { color: #aaa; }
    .dir-badge {
        font-size: 0.7rem;
        font-weight: 700;
        text-transform: uppercase;
        padding: 0.15rem 0.5rem;
        border-radius: 4px;
        flex-shrink: 0;
    }
    .dir-badge.inbound  { background: #1a2a3a; color: #93c5fd; }
    .dir-badge.outbound { background: #1a3a1a; color: #4ade80; }
    .dir-badge.other    { background: #2a2a2a; color: #aaa; }

    .thread-from { font-size: 0.85rem; color: #ccc; flex: 1; font-family: monospace; }
    .thread-date { font-size: 0.8rem; color: #666; font-family: monospace; margin-left: auto; white-space: nowrap; }

    .thread-body-wrapper { overflow: auto; max-height: none; position: relative; }
    .thread-body-wrapper.collapsed {
        max-height: 4.5em;
        overflow: hidden;
        cursor: pointer;
    }
    .thread-fade {
        position: absolute;
        bottom: 0;
        left: 0;
        right: 0;
        height: 2.5em;
        background: linear-gradient(transparent, #1e1e1e);
        pointer-events: none;
    }

    /* HTML email body */
    .thread-html-body {
        padding: 1rem 1.25rem;
        font-size: 0.9rem;
        line-height: 1.6;
        color: #ddd;
        word-wrap: break-word;
        overflow-wrap: break-word;
    }
    :global(.thread-html-body img) { max-width: 100%; height: auto; }
    :global(.thread-html-body table) { max-width: 100%; overflow-x: auto; display: block; }
    :global(.thread-html-body a) { color: #93c5fd; }
    :global(.thread-html-body blockquote) {
        border-left: 3px solid #444;
        margin: 0.5rem 0;
        padding: 0.25rem 0.75rem;
        color: #888;
    }

    .thread-empty { padding: 1rem 1.25rem; color: #555; font-style: italic; }

    /* Attachments */
    .attachment-list {
        display: flex;
        flex-wrap: wrap;
        gap: 0.6rem;
        padding: 0.75rem 1rem;
        border-top: 1px solid #2a2a2a;
        background: #1a1a1a;
    }
    .attachment-item {
        display: flex;
        align-items: center;
        gap: 0.4rem;
        background: #252525;
        border: 1px solid #333;
        border-radius: 6px;
        padding: 0.4rem 0.7rem;
        text-decoration: none;
        color: #ccc;
        font-size: 0.8rem;
        transition: background 0.15s, border-color 0.15s;
    }
    .attachment-item:hover { background: #2a2a2a; border-color: #4a69bd; color: #93c5fd; }
    .att-thumb {
        width: 40px;
        height: 40px;
        object-fit: cover;
        border-radius: 3px;
        border: 1px solid #333;
    }
    .att-icon { font-size: 1.1rem; }
    .att-label { font-family: monospace; font-size: 0.75rem; color: #888; }

    /* Header action buttons */
    .header-actions { display: flex; gap: 0.5rem; margin-left: auto; flex-wrap: wrap; }
    .ai-btn {
        background: #2a1a4a;
        color: #d8b4fe;
        border: 1px solid #a855f7;
        border-radius: 6px;
        padding: 0.4rem 0.9rem;
        font-size: 0.82rem;
        font-weight: 600;
        cursor: pointer;
        transition: background 0.2s;
    }
    .ai-btn:hover:not(:disabled) { background: #4a1d6e; }
    .ai-btn:disabled { opacity: 0.45; cursor: not-allowed; }
    .spinner { display: inline-block; animation: spin 1s linear infinite; }
    @keyframes spin { to { transform: rotate(360deg); } }

    .rma-btn {
        background: #1a3a1a;
        color: #4ade80;
        border: 1px solid #22c55e;
        border-radius: 6px;
        padding: 0.4rem 0.9rem;
        font-size: 0.82rem;
        font-weight: 600;
        cursor: pointer;
        transition: background 0.2s;
    }
    .rma-btn:hover:not(:disabled) { background: #14532d; }
    .rma-btn:disabled { opacity: 0.45; cursor: not-allowed; }

    .repair-btn {
        background: #1a2a3a;
        color: #93c5fd;
        border: 1px solid #3b82f6;
        border-radius: 6px;
        padding: 0.4rem 0.9rem;
        font-size: 0.82rem;
        font-weight: 600;
        cursor: pointer;
        transition: background 0.2s;
    }
    .repair-btn:hover:not(:disabled) { background: #1e3a5f; }
    .repair-btn:disabled { opacity: 0.45; cursor: not-allowed; }

    .copy-ai-btn {
        background: #2a2a2a;
        color: #ccc;
        border: 1px solid #444;
        border-radius: 6px;
        padding: 0.4rem 0.9rem;
        font-size: 0.82rem;
        font-weight: 600;
        cursor: pointer;
        transition: all 0.2s;
    }
    .copy-ai-btn:hover:not(:disabled) {
        background: #3a3a3a;
        color: #fff;
        border-color: #666;
    }
    .copy-ai-btn:disabled { opacity: 0.45; cursor: not-allowed; }

    /* AI Summary panel */
    .summary-panel {
        background: #1a1130;
        border: 1px solid #7c3aed;
        border-radius: 10px;
        padding: 1.25rem 1.5rem;
        margin-bottom: 1.5rem;
    }
    .summary-title {
        font-size: 0.75rem;
        font-weight: 700;
        text-transform: uppercase;
        color: #a855f7;
        letter-spacing: 0.5px;
        margin-bottom: 0.75rem;
    }
    .summary-loading { color: #888; font-style: italic; }
    .summary-error { color: #fbbf24; font-size: 0.85rem; }
    .summary-text {
        color: #e2d9f3;
        font-size: 0.9rem;
        line-height: 1.7;
        white-space: pre-wrap;
    }
    .use-as-issue-btn {
        margin-top: 0.75rem;
        background: none;
        border: none;
        color: #a855f7;
        font-size: 0.82rem;
        cursor: pointer;
        padding: 0;
        text-decoration: underline;
    }
    .use-as-issue-btn:hover { color: #d8b4fe; }
    .summary-actions { display: flex; gap: 1rem; margin-top: 0.75rem; }

    /* Semantic Matches */
    .semantic-matches-panel {
        background: #0f1a2e;
        border: 1px solid #1e40af;
        border-radius: 10px;
        padding: 1.25rem 1.5rem;
        margin-bottom: 1.5rem;
    }
    .semantic-title {
        font-size: 0.75rem;
        font-weight: 700;
        text-transform: uppercase;
        color: #60a5fa;
        letter-spacing: 0.5px;
        margin-bottom: 0.75rem;
    }
    .semantic-loading { color: #888; font-style: italic; font-size: 0.85rem; }
    .semantic-list { display: flex; flex-direction: column; gap: 0.5rem; }
    .semantic-item {
        display: flex;
        align-items: center;
        gap: 0.75rem;
        background: #1a2744;
        border: 1px solid #1e3a5f;
        border-radius: 6px;
        padding: 0.5rem 0.75rem;
        text-decoration: none;
        color: #ccc;
        font-size: 0.85rem;
        transition: background 0.15s, border-color 0.15s;
    }
    .semantic-item:hover { background: #1e3a5f; border-color: #3b82f6; color: #fff; }
    .sem-type-badge {
        font-size: 0.65rem;
        font-weight: 700;
        text-transform: uppercase;
        padding: 0.15rem 0.5rem;
        border-radius: 4px;
        flex-shrink: 0;
    }
    .sem-ticket { background: #1a2a3a; color: #93c5fd; }
    .sem-repair { background: #1a3a1a; color: #4ade80; }
    .sem-label { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .sem-score {
        font-family: monospace;
        font-size: 0.8rem;
        color: #60a5fa;
        flex-shrink: 0;
    }
</style>
