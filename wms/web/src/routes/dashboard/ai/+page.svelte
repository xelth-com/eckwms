<script>
    import { onMount } from 'svelte';
    import { api } from '$lib/api';
    import { toastStore } from '$lib/stores/toastStore';
    import { wsStore } from '$lib/stores/wsStore';

    let tasks = [];
    let loading = true;
    let error = null;
    let replies = {};
    let submitting = {};
    let lastHandledWsAt = 0;

    onMount(loadTasks);

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
            toastStore.add('New AI task requires your attention', 'info', 5000);
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
            toastStore.add('Failed to load AI tasks', 'error');
        } finally {
            loading = false;
        }
    }

    async function submitReply(taskId) {
        const message = replies[taskId]?.trim();
        if (!message) {
            toastStore.add('Please enter a reply', 'warning');
            return;
        }

        submitting[taskId] = true;
        try {
            await api.post(`/api/ai/tasks/${taskId}/reply`, { message });
            toastStore.add('Reply sent to AI', 'success');
            tasks = tasks.filter(t => t.id !== taskId);
            delete replies[taskId];
            delete submitting[taskId];
        } catch (e) {
            toastStore.add(`Failed to send reply: ${e.message}`, 'error');
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
            'The AI paused but provided no specific question.'
        );
    }
</script>

<div class="ai-inbox-page">
    <header>
        <h1>AI Operator Inbox</h1>
        <button class="action-btn" on:click={loadTasks} disabled={loading}>
            {loading ? 'Refreshing…' : 'Refresh'}
        </button>
    </header>

    <div class="page-desc">
        Tasks requiring human intervention. The AI Brain has paused execution
        and is waiting for your input to proceed.
    </div>

    {#if loading && tasks.length === 0}
        <div class="loading-state">Loading pending tasks…</div>
    {:else if error}
        <div class="error-state">Error: {error}</div>
    {:else if tasks.length === 0}
        <div class="empty-state">
            <span class="icon">✨</span>
            <p>All clear. No tasks are waiting for human input.</p>
        </div>
    {:else}
        <div class="task-grid">
            {#each tasks as task (task.id)}
                <div class="task-card">
                    <div class="card-header">
                        <span class="task-id" title={task.id}>
                            Task: {shortId(task.id)}
                        </span>
                        <span class="time">{formatTime(task.updated_at)}</span>
                    </div>

                    <div class="context-box">
                        <div class="box-label">Context</div>
                        <pre>{JSON.stringify(task.context ?? {}, null, 2)}</pre>
                    </div>

                    <div class="question-box">
                        <div class="box-label highlight">AI Question</div>
                        <p>{questionFor(task)}</p>
                    </div>

                    <div class="reply-section">
                        <textarea
                            bind:value={replies[task.id]}
                            placeholder="Type your reply to the AI here…"
                            rows="3"
                            disabled={submitting[task.id]}
                        ></textarea>
                        <button
                            class="submit-btn"
                            on:click={() => submitReply(task.id)}
                            disabled={submitting[task.id] || !replies[task.id]?.trim()}
                        >
                            {submitting[task.id] ? 'Sending…' : 'Send Reply'}
                        </button>
                    </div>
                </div>
            {/each}
        </div>
    {/if}
</div>

<style>
    .ai-inbox-page {
        max-width: 1000px;
        margin: 0 auto;
        padding-bottom: 2rem;
    }

    header {
        display: flex;
        justify-content: space-between;
        align-items: center;
        margin-bottom: 0.5rem;
    }

    h1 { color: #fff; font-size: 1.8rem; margin: 0; }

    .page-desc {
        color: #888;
        font-size: 0.95rem;
        margin-bottom: 2rem;
        padding-bottom: 1rem;
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
    }
    .action-btn:hover:not(:disabled) { background: #4a69bd; color: white; }
    .action-btn:disabled { opacity: 0.5; cursor: not-allowed; }

    .loading-state, .error-state, .empty-state {
        text-align: center;
        padding: 4rem;
        background: #1e1e1e;
        border-radius: 8px;
        border: 1px solid #333;
        color: #aaa;
    }

    .error-state { color: #ff6b6b; border-color: #ff6b6b; }

    .empty-state .icon { font-size: 3rem; display: block; margin-bottom: 1rem; }
    .empty-state p { font-size: 1.1rem; color: #ccc; margin: 0; }

    .task-grid {
        display: flex;
        flex-direction: column;
        gap: 1.5rem;
    }

    .task-card {
        background: #1e1e1e;
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
</style>
