<script>
    import { onMount } from 'svelte';
    import { base } from '$app/paths';
    import { api } from '$lib/api';
    import { authStore } from '$lib/stores/authStore';
    import { toastStore } from '$lib/stores/toastStore';
    import { wsStore } from '$lib/stores/wsStore';
    import { t, tr } from '$lib/i18n';
    import ChatPanel from '$lib/components/ChatPanel.svelte';

    let tasks = [];
    let results = [];
    let loading = true;
    let error = null;
    let replies = {};
    let submitting = {};
    let lastHandledWsAt = 0;

    // Right-column tabbed panel. Driven by this array so a new tab is a
    // one-entry addition (id + i18n label key) — no template restructuring.
    const rightPanelTabs = [
        { id: 'inbox', label: 'ai.inbox_tab_label' },
        { id: 'context', label: 'ai.context_tab_label' },
        { id: 'history', label: 'ai.history_tab_label' },
        { id: 'preview', label: 'ai.preview_tab_label' },
    ];
    let activeTab = 'inbox';

    // History tab: ChatPanel owns the actual conversation state (it persists
    // to localStorage itself); it dispatches `state` on every change so this
    // page can render a summary line without duplicating that state.
    let historySessionId = null;
    let historyTurns = 0;
    let historySummary = null;
    function onChatState(e) {
        historySessionId = e.detail.sessionId;
        historyTurns = e.detail.turns;
        historySummary = e.detail.summary;
    }

    onMount(() => {
        loadTasks();
        loadResults();
    });

    // ── Resizable split between the chat and the side panel ────────────────
    // Percentage of the panes' width given to the chat column; persisted so
    // the operator's layout survives reloads. Clamped so neither side can be
    // dragged shut.
    const SPLIT_KEY = 'eck_brain_split_pct';
    const SPLIT_MIN = 25;
    const SPLIT_MAX = 75;
    let splitPct = 55;
    try {
        if (typeof localStorage !== 'undefined') {
            const saved = Number(localStorage.getItem(SPLIT_KEY));
            if (Number.isFinite(saved) && saved >= SPLIT_MIN && saved <= SPLIT_MAX) splitPct = saved;
        }
    } catch (e) {
        console.error('split restore', e);
    }
    let panesEl = null;
    let dragging = false;

    function onSplitPointerDown(e) {
        if (!panesEl) return;
        dragging = true;
        e.currentTarget.setPointerCapture(e.pointerId);
        e.preventDefault();
    }
    function onSplitPointerMove(e) {
        if (!dragging || !panesEl) return;
        const rect = panesEl.getBoundingClientRect();
        if (rect.width <= 0) return;
        const pct = ((e.clientX - rect.left) / rect.width) * 100;
        splitPct = Math.min(SPLIT_MAX, Math.max(SPLIT_MIN, Math.round(pct * 10) / 10));
    }
    function onSplitPointerUp(e) {
        if (!dragging) return;
        dragging = false;
        try { e.currentTarget.releasePointerCapture(e.pointerId); } catch { /* already released */ }
        try { localStorage.setItem(SPLIT_KEY, String(splitPct)); } catch (err) { console.error('split save', err); }
    }
    // Keyboard nudge for the separator (a11y): ←/→ move it by 2%.
    function onSplitKeydown(e) {
        if (e.key !== 'ArrowLeft' && e.key !== 'ArrowRight') return;
        e.preventDefault();
        const delta = e.key === 'ArrowLeft' ? -2 : 2;
        splitPct = Math.min(SPLIT_MAX, Math.max(SPLIT_MIN, splitPct + delta));
        try { localStorage.setItem(SPLIT_KEY, String(splitPct)); } catch (err) { console.error('split save', err); }
    }

    // When the orchestrator broadcasts AI_TASK_PAUSED (fired by AskHumanTool),
    // reload so the new card appears immediately. We dedupe on `_receivedAt`
    // because the wsStore holds the message after the socket closes and
    // reactive blocks fire again on any unrelated store dependency change.
    $: {
        const msg = $wsStore.lastMessage;
        if (
            msg &&
            msg.type === 'AI_TASK_PAUSED' &&
            msg._receivedAt &&
            msg._receivedAt > lastHandledWsAt &&
            Date.now() - msg._receivedAt < 2000
        ) {
            lastHandledWsAt = msg._receivedAt;
            toastStore.add(tr('ai.new_task_notify'), 'info', 5000);
            loadTasks();
        }
    }

    async function loadTasks() {
        loading = true;
        error = null;
        try {
            const result = await api.get('/api/ai/tasks?state=awaiting_human');
            tasks = Array.isArray(result) ? result : [];
            for (const t of tasks) {
                if (replies[t.id] === undefined) replies[t.id] = '';
                submitting[t.id] = false;
            }
        } catch (e) {
            console.error(e);
            error = e.message;
            toastStore.add(tr('ai.load_failed'), 'error');
        } finally {
            loading = false;
        }
    }

    // Completed triage runs — the other half of the loop. The orchestrator
    // now persists its final answer on the task row (`result`), so operators
    // can read what the AI concluded instead of the verdict dying in the
    // thought log. Failed runs are shown too (collapsed to their error).
    async function loadResults() {
        try {
            const done = await api.get('/api/ai/tasks?state=completed');
            const failed = await api.get('/api/ai/tasks?state=failed');
            results = [...(Array.isArray(done) ? done : []), ...(Array.isArray(failed) ? failed : [])]
                .sort((a, b) => String(b.updated_at).localeCompare(String(a.updated_at)))
                .slice(0, 20);
        } catch (e) {
            console.error('loadResults', e);
        }
    }

    function ticketOf(task) {
        return task.context?.ticket_id || null;
    }

    function ticketNumberOf(task) {
        return task.context?.meta?.ticket_number || null;
    }

    function subjectOf(task) {
        return task.context?.meta?.subject || '';
    }

    async function submitReply(taskId) {
        const message = replies[taskId]?.trim();
        if (!message) {
            toastStore.add(tr('ai.enter_reply'), 'warning');
            return;
        }

        submitting[taskId] = true;
        try {
            await api.post(`/api/ai/tasks/${taskId}/reply`, { message });
            toastStore.add(tr('ai.reply_sent'), 'success');
            tasks = tasks.filter(t => t.id !== taskId);
            delete replies[taskId];
            delete submitting[taskId];
        } catch (e) {
            toastStore.add(tr('ai.reply_failed', { error: e.message }), 'error');
            submitting[taskId] = false;
        }
    }

    function formatTime(dateStr) {
        if (!dateStr) return '-';
        try {
            return new Date(dateStr).toLocaleString('de-DE');
        } catch {
            return String(dateStr);
        }
    }

    function shortId(id) {
        if (!id) return '?';
        const tail = id.split(':').pop() || id;
        return tail.length > 8 ? `${tail.substring(0, 8)}…` : tail;
    }

    function questionFor(task) {
        return (
            task.awaiting_input_schema?.question ||
            tr('ai.no_question')
        );
    }
</script>


<!-- Layout: one grid, three columns, one full-height row. Left = the Brain
     Chat conversation (ChatPanel, which owns its own header/log/composer);
     middle = the draggable separator; right = the tabbed side panel (Operator
     Inbox / results / context / history / preview). The column template is
     dragged by the splitter. The page is absolutely positioned inside the
     layout's .content so it fills the pane instead of scrolling with it. -->
<div
    class="ai-page"
    bind:this={panesEl}
    class:dragging
    style="--split: {splitPct}%"
>
    <div class="chat-pane">
        <ChatPanel context={null} compact={false} on:state={onChatState} />
    </div>

    <!-- Drag handle. The width reference for the drag math is the whole grid
         (panesEl = the page element). A div with role=separator is the WAI-ARIA
         pattern for a window splitter; svelte's a11y lint doesn't know it. -->
    <!-- svelte-ignore a11y_no_noninteractive_tabindex a11y_no_noninteractive_element_interactions -->
    <div
        class="splitter"
        role="separator"
        aria-orientation="vertical"
        aria-valuenow={Math.round(splitPct)}
        aria-valuemin={SPLIT_MIN}
        aria-valuemax={SPLIT_MAX}
        tabindex="0"
        on:pointerdown={onSplitPointerDown}
        on:pointermove={onSplitPointerMove}
        on:pointerup={onSplitPointerUp}
        on:pointercancel={onSplitPointerUp}
        on:keydown={onSplitKeydown}
    ></div>

    <div class="side-pane">
        <div class="tab-bar" role="tablist">
            {#each rightPanelTabs as tabDef (tabDef.id)}
                <button
                    type="button"
                    class="tab-btn"
                    class:active={activeTab === tabDef.id}
                    role="tab"
                    aria-selected={activeTab === tabDef.id}
                    on:click={() => (activeTab = tabDef.id)}
                >
                    {$t(tabDef.label)}
                </button>
            {/each}
        </div>

        <div class="tab-content">
            {#if activeTab === 'inbox'}
                <div class="inbox-toolbar">
                    <button class="action-btn small" on:click={loadTasks} disabled={loading} title={$t('ai.refresh')}>
                        {loading ? $t('ai.refreshing') : $t('ai.refresh')}
                    </button>
                </div>
                {#if loading && tasks.length === 0}
                    <div class="loading-state">{$t('ai.loading_tasks')}</div>
                {:else if error}
                    <div class="error-state">{$t('ai.error_generic', { error })}</div>
                {:else if tasks.length === 0}
                    <div class="empty-state">
                        <span class="icon">✨</span>
                        <p>{$t('ai.empty_state')}</p>
                    </div>
                {:else}
                    <div class="task-grid">
                        {#each tasks as task (task.id)}
                            <div class="task-card">
                                <div class="card-header">
                                    <span class="task-id" title={task.id}>
                                        {$t('ai.task_label', { id: shortId(task.id) })}
                                    </span>
                                    <span class="time">{formatTime(task.updated_at)}</span>
                                </div>

                                <div class="context-box">
                                    <div class="box-label">{$t('ai.context_label')}</div>
                                    <pre>{JSON.stringify(task.context ?? {}, null, 2)}</pre>
                                </div>

                                <div class="question-box">
                                    <div class="box-label highlight">{$t('ai.ai_question_label')}</div>
                                    <p>{questionFor(task)}</p>
                                </div>

                                <div class="reply-section">
                                    <textarea
                                        bind:value={replies[task.id]}
                                        placeholder={$t('ai.reply_placeholder')}
                                        rows="3"
                                        disabled={submitting[task.id] || $authStore?.isKioskObserver}
                                    ></textarea>
                                    <button
                                        class="submit-btn"
                                        on:click={() => submitReply(task.id)}
                                        disabled={submitting[task.id] || !replies[task.id]?.trim() || $authStore?.isKioskObserver}
                                    >
                                        {submitting[task.id] ? $t('ai.sending') : $t('ai.send_reply')}
                                    </button>
                                </div>
                            </div>
                        {/each}
                    </div>
                {/if}

                <section class="results-section">
                    <h2>{$t('ai.results_title')}</h2>
                    <div class="page-desc">{$t('ai.results_desc')}</div>
                    {#if results.length === 0}
                        <div class="empty-state small">
                            <p>{$t('ai.no_results')}</p>
                        </div>
                    {:else}
                        <div class="result-list">
                            {#each results as r (r.id)}
                                <div class="result-card" class:failed={r.state === 'failed'}>
                                    <div class="card-header">
                                        {#if ticketOf(r)}
                                            <a href="{base}/dashboard/support/{ticketOf(r)}" class="ticket-link">
                                                {ticketNumberOf(r)
                                                    ? tr('ai.ticket_label', { n: ticketNumberOf(r) })
                                                    : tr('ai.task_label', { id: shortId(r.id) })}
                                            </a>
                                        {:else}
                                            <span class="task-id">{tr('ai.task_label', { id: shortId(r.id) })}</span>
                                        {/if}
                                        <span class="time">{formatTime(r.updated_at)}</span>
                                    </div>
                                    {#if subjectOf(r)}
                                        <div class="subject">{subjectOf(r)}</div>
                                    {/if}
                                    <div class="verdict">
                                        <div class="box-label" class:highlight={r.state !== 'failed'}>
                                            {r.state === 'failed' ? $t('ai.failed_label') : $t('ai.result_label')}
                                        </div>
                                        <p>{r.state === 'failed' ? (r.error || tr('ai.no_result_text')) : (r.result || tr('ai.no_result_text'))}</p>
                                    </div>
                                </div>
                            {/each}
                        </div>
                    {/if}
                </section>
            {:else if activeTab === 'context'}
                <!-- Placeholder: this is where per-turn tool calls and cited
                     tickets/docs from the current Brain Chat answer will
                     render once the backend surfaces that data. No wiring
                     yet — empty-state only. -->
                <div class="empty-state small">
                    <p>{$t('ai.context_empty')}</p>
                </div>
            {:else if activeTab === 'history'}
                <!-- v1: the Brain Chat's running conversation lives inside
                     ChatPanel (localStorage-persisted per user), which
                     dispatches `state` on every change — list that single
                     session here. Structured as a list so multi-session
                     history (server-side session list) can populate this
                     later without changing the tab shell. -->
                {#if historySessionId || historyTurns > 0}
                    <div class="history-list">
                        <div class="history-item">
                            <span class="history-id">{historySessionId ? shortId(historySessionId) : $t('ai.history_tab_label')}</span>
                            <span class="history-meta">{historyTurns} {historyTurns === 1 ? 'message' : 'messages'}</span>
                        </div>
                    </div>
                {:else}
                    <div class="empty-state small">
                        <p>{$t('ai.history_empty')}</p>
                    </div>
                {/if}
            {:else if activeTab === 'preview'}
                <!-- Placeholder: images/documents cited by the agent (attachments,
                     screenshots, PDFs referenced during a run) will preview here
                     once the backend surfaces attachment refs per turn. -->
                <div class="empty-state small">
                    <p>{$t('ai.preview_empty')}</p>
                </div>
            {/if}
        </div>
    </div>
</div>

<style>
    /* Fill the layout pane (position:relative .content) edge-to-edge, ignoring
       its padding, so the panes sit at the bottom of the viewport and only the
       two panes scroll. */
    .ai-page {
        position: absolute;
        inset: 0.75rem 1.25rem 1rem 1.25rem;
        display: grid;
        grid-template-columns: var(--split) 10px minmax(0, 1fr);
        grid-template-rows: minmax(0, 1fr);
        column-gap: 0;
        min-height: 0;
    }
    .ai-page.dragging { user-select: none; cursor: col-resize; }

    .chat-pane {
        grid-column: 1;
        grid-row: 1;
        min-width: 0;
        min-height: 0;
    }

    .splitter {
        grid-column: 2;
        grid-row: 1;
        cursor: col-resize;
        position: relative;
        touch-action: none;
        outline: none;
    }
    .splitter::before {
        content: '';
        position: absolute;
        top: 0;
        bottom: 0;
        left: 50%;
        width: 2px;
        margin-left: -1px;
        background: #333;
        border-radius: 1px;
        transition: background 0.15s;
    }
    .splitter:hover::before,
    .splitter:focus-visible::before,
    .ai-page.dragging .splitter::before { background: #4a69bd; }

    .side-pane {
        grid-column: 3;
        grid-row: 1;
        display: flex;
        flex-direction: column;
        min-height: 0;
        min-width: 0;
    }

    .tab-bar {
        display: flex;
        gap: 0.25rem;
        padding: 0 0.5rem;
        align-items: flex-end;
        border-bottom: 1px solid #333;
        min-width: 0;
        overflow-x: auto;
        flex: 0 0 auto;
    }
    .tab-btn {
        background: transparent;
        border: 1px solid transparent;
        border-bottom: none;
        color: #aaa;
        padding: 0.55rem 0.9rem;
        border-radius: 6px 6px 0 0;
        font-size: 0.9rem;
        font-weight: 600;
        cursor: pointer;
        transition: all 0.2s;
        white-space: nowrap;
    }
    .tab-btn:hover { color: #e0e0e0; }
    .tab-btn.active {
        color: #fff;
        background: #2a2a2a;
        border-color: #444;
    }

    .tab-content {
        flex: 1 1 auto;
        min-height: 0;
        min-width: 0;
        overflow-y: auto;
        padding: 1rem;
        margin-top: 0.75rem;
        background: #1e1e1e;
        border: 1px solid #333;
        border-radius: 8px;
    }

    /* ── Shared bits (inbox cards, results, states) ──────────────────────── */
    .inbox-toolbar {
        display: flex;
        justify-content: flex-end;
        margin-bottom: 0.75rem;
    }

    .page-desc {
        color: #888;
        font-size: 0.95rem;
        margin-bottom: 1rem;
        padding-bottom: 0.75rem;
        border-bottom: 1px solid #333;
    }

    .action-btn {
        padding: 0.6rem 1.2rem;
        border-radius: 4px;
        border: 1px solid #4a69bd;
        background: transparent;
        color: #4a69bd;
        font-weight: 600;
        cursor: pointer;
        transition: all 0.2s;
        white-space: nowrap;
    }
    .action-btn.small { padding: 0.35rem 0.8rem; font-size: 0.85rem; }
    .action-btn:hover:not(:disabled) { background: #4a69bd; color: white; }
    .action-btn:disabled { opacity: 0.5; cursor: not-allowed; }

    .loading-state, .error-state, .empty-state {
        text-align: center;
        padding: 3rem 1.5rem;
        background: #181818;
        border-radius: 8px;
        border: 1px solid #333;
        color: #aaa;
    }
    .error-state { color: #ff6b6b; border-color: #ff6b6b; }
    .empty-state .icon { font-size: 3rem; display: block; margin-bottom: 1rem; }
    .empty-state p { font-size: 1.1rem; color: #ccc; margin: 0; }
    .empty-state.small { padding: 1.5rem; }

    .task-grid {
        display: flex;
        flex-direction: column;
        gap: 1.5rem;
    }
    .task-card {
        background: #1a1a1a;
        border: 1px solid #3b82f6;
        border-radius: 8px;
        padding: 1.5rem;
        display: flex;
        flex-direction: column;
        gap: 1rem;
        box-shadow: 0 4px 12px rgba(0, 0, 0, 0.2);
    }
    .card-header {
        display: flex;
        justify-content: space-between;
        align-items: center;
        border-bottom: 1px solid #333;
        padding-bottom: 0.75rem;
    }
    .task-id {
        font-family: monospace;
        color: #60a5fa;
        font-weight: 600;
        background: #111827;
        padding: 0.2rem 0.5rem;
        border-radius: 4px;
        border: 1px solid #1e3a8a;
    }
    .time { color: #666; font-size: 0.85rem; }

    .box-label {
        font-size: 0.75rem;
        text-transform: uppercase;
        font-weight: 700;
        color: #888;
        margin-bottom: 0.5rem;
        letter-spacing: 0.5px;
    }
    .box-label.highlight { color: #a78bfa; }

    .context-box {
        background: #121212;
        border: 1px solid #2a2a2a;
        border-radius: 6px;
        padding: 0.75rem;
    }
    .context-box pre {
        margin: 0;
        color: #a3bffa;
        font-size: 0.8rem;
        white-space: pre-wrap;
        word-break: break-all;
    }

    .question-box {
        background: rgba(139, 92, 246, 0.1);
        border-left: 4px solid #8b5cf6;
        border-radius: 4px;
        padding: 1rem;
    }
    .question-box p {
        margin: 0;
        color: #e2d9f3;
        font-size: 1.05rem;
        line-height: 1.5;
    }

    .reply-section {
        display: flex;
        flex-direction: column;
        gap: 0.75rem;
        margin-top: 0.5rem;
    }

    textarea {
        background: #121212;
        border: 1px solid #444;
        border-radius: 6px;
        padding: 0.8rem;
        color: #fff;
        font-size: 1rem;
        font-family: inherit;
        resize: vertical;
        transition: border-color 0.2s;
    }
    textarea:focus { border-color: #4a69bd; outline: none; }
    textarea:disabled { opacity: 0.5; cursor: not-allowed; }

    .submit-btn {
        align-self: flex-end;
        background: #28a745;
        color: white;
        border: none;
        padding: 0.8rem 2rem;
        border-radius: 6px;
        font-weight: 600;
        font-size: 0.95rem;
        cursor: pointer;
        transition: background 0.2s;
    }
    .submit-btn:hover:not(:disabled) { background: #218838; }
    .submit-btn:disabled { background: #333; color: #666; cursor: not-allowed; }

    .history-list { display: flex; flex-direction: column; gap: 0.6rem; }
    .history-item {
        display: flex;
        justify-content: space-between;
        align-items: center;
        background: #121212;
        border: 1px solid #2a2a2a;
        border-radius: 6px;
        padding: 0.7rem 0.9rem;
    }
    .history-id { font-family: monospace; color: #60a5fa; }
    .history-meta { color: #777; font-size: 0.8rem; }

    .results-section { margin-top: 2.5rem; }
    .results-section h2 { margin: 0 0 0.3rem; color: #fff; font-size: 1.1rem; }
    .result-list { display: flex; flex-direction: column; gap: 0.8rem; }
    .result-card {
        background: #1c1c1c;
        border: 1px solid #333;
        border-left: 3px solid #28a745;
        border-radius: 8px;
        padding: 0.9rem 1.1rem;
    }
    .result-card.failed { border-left-color: #dc3545; }
    .result-card .subject { color: #ccc; font-weight: 600; margin: 0.4rem 0; }
    .result-card .verdict p { white-space: pre-wrap; color: #ddd; margin: 0.3rem 0 0; }
    .ticket-link { color: #6c9bd8; font-weight: 600; text-decoration: none; }
    .ticket-link:hover { text-decoration: underline; }

    /* ── Narrow screens: stack the panes, drop the drag handle ───────────── */
    @media (max-width: 900px) {
        .ai-page {
            grid-template-columns: minmax(0, 1fr);
            grid-template-rows: minmax(0, 1fr) minmax(0, 1fr);
        }
        .chat-pane { grid-column: 1; grid-row: 1; }
        .splitter { display: none; }
        .side-pane { grid-column: 1; grid-row: 2; }
    }
</style>
