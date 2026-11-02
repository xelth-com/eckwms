<script>
    // Brain Chat conversation, extracted from routes/dashboard/ai/+page.svelte
    // so it can be reused both full-size (the /dashboard/ai page) and compact
    // (embedded on an entity object page). It owns the whole conversation:
    // engine picker, the message log, streaming + takeover handling, and the
    // composer. The surrounding page keeps everything else (Operator Inbox,
    // results, tabs, the resizable split).
    //
    // Props:
    //   context  null | { kind, id }  — when set, the request carries `context`
    //            so the server scopes the Brain to that entity, and the visible
    //            history is kept under a per-entity localStorage key, separate
    //            from the main conversation.
    //   compact  bool — tighter spacing/fonts for an embedded panel.
    import { tick, createEventDispatcher } from 'svelte';
    import { get } from 'svelte/store';
    import { api } from '$lib/api';
    import { authStore } from '$lib/stores/authStore';
    import { toastStore } from '$lib/stores/toastStore';
    import { t, tr } from '$lib/i18n';
    import { renderChatText } from '$lib/chatMarkdown';
    import { createSseRecordSplitter, parseSseRecord } from '$lib/sseParser';
    import {
        CHAT_ENGINE_KEY, CHAT_HISTORY_KEY, CHAT_SUMMARY_KEY, CHAT_SESSION_KEY,
        usernameOf, chatStorageKey, dropLegacyChatKeys, clearChatConversationFor,
    } from '$lib/chatStorage';

    export let context = null; // null | { kind, id }
    export let compact = false;

    const dispatch = createEventDispatcher();

    // Storage bases. In context mode the visible transcript + its rolling
    // summary live under a per-entity key (`eck_chat_ctx_<kind>_<id>`), so an
    // object page's conversation starts empty and never mixes with the main
    // chat. The telemetry session id stays per-user in both modes (§6), as does
    // the engine preference.
    const ctxBase = context ? `eck_chat_ctx_${context.kind}_${context.id}` : null;
    const HISTORY_BASE = ctxBase || CHAT_HISTORY_KEY;
    const SUMMARY_BASE = ctxBase ? `${ctxBase}_summary` : CHAT_SUMMARY_KEY;
    const SESSION_BASE = CHAT_SESSION_KEY;

    let chatMessages = []; // { role: 'user'|'assistant', text, model? }
    let chatInput = '';
    let chatSending = false;
    let chatEngine = 'glm';
    let chatStatus = ''; // live status line while a streamed answer is in flight
    let chatOffer = null; // {from, to} when the server offered a Gemini takeover
    let chatHydrated = false; // gate persistence until the initial restore ran
    // Run-generation guard: each runChat() call stamps its own generation
    // number. Only the CURRENT generation is allowed to touch chatSending /
    // chatStatus in its `finally` or apply a result/error — an aborted
    // (superseded) request must be silent.
    let chatGen = 0;
    let chatController = null; // AbortController for the in-flight stream fetch
    let pendingChat = null; // { question, history } for the in-flight request, so a takeover can re-ask
    let chatSummary = null; // compacted running summary (server-managed rolling compaction)
    let chatSessionId = null; // transcript-telemetry session id (server-minted)

    // Per-user namespacing (kiosk browsers are shared): every persisted chat
    // key is suffixed with the authenticated username so the next account on
    // the same browser never restores — and re-sends — the previous account's
    // conversation. `identityUser` is `undefined` until the auth store has
    // resolved at least once, `null` for no authenticated user.
    let identityUser = undefined;

    function restoreChatFor(username) {
        chatHydrated = false;
        chatMessages = [];
        chatSummary = null;
        chatSessionId = null;
        chatEngine = 'glm';
        chatInput = '';
        if (username && typeof localStorage !== 'undefined') {
            try {
                const saved = localStorage.getItem(chatStorageKey(CHAT_ENGINE_KEY, username));
                if (['glm', 'gemini', 'deepseek', 'mimo-pro', 'mimo-flash'].includes(saved)) chatEngine = saved;
                const raw = localStorage.getItem(chatStorageKey(HISTORY_BASE, username));
                if (raw) {
                    const parsed = JSON.parse(raw);
                    if (Array.isArray(parsed)) {
                        chatMessages = parsed
                            .filter(m => m && (m.role === 'user' || m.role === 'assistant') && typeof m.text === 'string')
                            .map(m => ({ role: m.role, text: m.text, ...(m.model ? { model: m.model } : {}), ...(m.usage ? { usage: m.usage } : {}) }));
                    }
                }
                const savedSummary = localStorage.getItem(chatStorageKey(SUMMARY_BASE, username));
                if (savedSummary) chatSummary = savedSummary;
                const savedSession = localStorage.getItem(chatStorageKey(SESSION_BASE, username));
                if (savedSession) chatSessionId = savedSession;
            } catch (e) {
                console.error('chat restore', e);
            }
        }
        chatHydrated = true;
    }

    $: {
        const u = $authStore.isLoading ? undefined : usernameOf($authStore.currentUser);
        if (u !== identityUser) {
            const first = identityUser === undefined;
            identityUser = u;
            if (!first) {
                if (chatController) chatController.abort();
                chatGen++;
                chatSending = false;
                chatStatus = '';
                chatOffer = null;
                pendingChat = null;
            }
            dropLegacyChatKeys();
            restoreChatFor(u);
        }
    }

    // ── Chat log auto-scroll ────────────────────────────────────────────────
    // NB: never write `chatLogEl.scrollTop = …` directly in a reactive block —
    // Svelte (legacy mode) treats a member assignment on a reactive variable as
    // an invalidation of `chatLogEl`, which re-triggers the block: an infinite
    // flush loop that freezes the tab. Go through a plain local.
    let chatLogEl = null;
    function scrollChatToBottom() {
        const el = chatLogEl;
        if (!el) return;
        tick().then(() => { el.scrollTop = el.scrollHeight; });
    }
    $: chatMessages, chatSending, chatStatus, chatOffer, chatLogEl, scrollChatToBottom();

    // Persist the last 50 messages (role/text/model only) on every change,
    // under this user's namespaced keys. The slice(-50) cap can land mid-round
    // (an orphaned assistant head with no preceding user turn) — trim down to
    // the next round boundary so what we persist is always re-sendable as
    // clean user/assistant pairs.
    function persistChatHistory() {
        if (!chatHydrated || !identityUser) return;
        try {
            let tail = chatMessages.slice(-50);
            while (tail.length && tail[0].role !== 'user') tail = tail.slice(1);
            const slim = tail
                .map(m => ({ role: m.role, text: m.text, ...(m.model ? { model: m.model } : {}), ...(m.usage ? { usage: m.usage } : {}) }));
            localStorage.setItem(chatStorageKey(HISTORY_BASE, identityUser), JSON.stringify(slim));
            if (chatSummary) localStorage.setItem(chatStorageKey(SUMMARY_BASE, identityUser), chatSummary);
            else localStorage.removeItem(chatStorageKey(SUMMARY_BASE, identityUser));
            if (chatSessionId) localStorage.setItem(chatStorageKey(SESSION_BASE, identityUser), chatSessionId);
        } catch (e) {
            console.error('chat history save', e);
        }
    }
    $: chatMessages, chatSummary, chatSessionId, persistChatHistory();

    // Surface the running conversation's session/turn count to the host page
    // (the /dashboard/ai History tab) so it can render a summary line without
    // duplicating this component's localStorage-backed state.
    $: dispatch('state', { sessionId: chatSessionId, turns: chatMessages.length, summary: chatSummary });

    // Apply a server response's rolling-compaction + session bookkeeping, then
    // append the assistant answer.
    function applyChatResult(payload) {
        if (payload.session_id) chatSessionId = payload.session_id;
        const clipped = typeof payload.history_clipped === 'number' ? payload.history_clipped : 0;
        if (clipped > 0) chatMessages = chatMessages.slice(clipped);
        if (payload.compaction && payload.compaction.summary) {
            chatSummary = payload.compaction.summary;
            const drop = typeof payload.compaction.dropped_messages === 'number'
                ? payload.compaction.dropped_messages
                : (payload.compaction.dropped_turns || 0) * 2;
            if (drop > 0) chatMessages = chatMessages.slice(drop);
        }
        chatMessages = [...chatMessages, {
            role: 'assistant',
            text: payload.answer,
            model: payload.model,
            hedge: (payload.hedged && payload.hedge_from) ? { from: payload.hedge_from } : null,
            fallback: payload.fallback_from ? { from: payload.fallback_from } : null,
            usage: payload.usage || null,
        }];
    }

    // "cache NN%" tag from a turn's usage (server-reported cache_ratio).
    function cacheTag(usage) {
        if (!usage || typeof usage.cache_ratio !== 'number') return '';
        return `cache ${Math.round(usage.cache_ratio * 100)}%`;
    }

    function authToken() {
        const state = get(authStore);
        return state.token || (typeof localStorage !== 'undefined' && localStorage.getItem('auth_token')) || null;
    }

    function saveChatEngine() {
        try {
            if (identityUser) localStorage.setItem(chatStorageKey(CHAT_ENGINE_KEY, identityUser), chatEngine);
        } catch (e) {
            console.error('chat engine save', e);
        }
    }

    // Blocking fallback — used when the streaming endpoint can't be reached
    // (non-2xx before any SSE event). Goes through the shared request helper so
    // it keeps the 401 refresh/retry and 403 OBSERVER_FORBIDDEN handling.
    async function sendChatBlocking(question, history, engine, takeoverFrom, gen, signal) {
        const payload = await api.post('/api/ai/chat', {
            question, history, engine, summary: chatSummary, session_id: chatSessionId,
            ...(takeoverFrom ? { takeover_from: takeoverFrom } : {}),
            ...(context ? { context } : {}),
        }, signal);
        if (gen !== chatGen) return; // superseded while this request was in flight
        applyChatResult(payload);
    }

    // The actual streaming request, shared by a normal send and a Gemini
    // takeover re-ask.
    async function runChat(question, history, engine, takeoverFrom, pushedQuestion = !takeoverFrom) {
        const gen = ++chatGen;
        const takeoverOffer = takeoverFrom ? { from: takeoverFrom, to: engine } : null;
        pendingChat = { question, history };
        chatSending = true;
        chatStatus = tr('ai.chat_thinking');
        chatOffer = null;

        const controller = new AbortController();
        chatController = controller;

        try {
            const token = authToken();
            const resp = await fetch('/api/ai/chat/stream', {
                method: 'POST',
                headers: {
                    'Content-Type': 'application/json',
                    ...(token ? { Authorization: `Bearer ${token}` } : {}),
                },
                body: JSON.stringify({
                    question, history, engine, summary: chatSummary, session_id: chatSessionId,
                    ...(takeoverFrom ? { takeover_from: takeoverFrom } : {}),
                    ...(context ? { context } : {}),
                }),
                signal: controller.signal,
            });

            if (!resp.ok || !resp.body) {
                await sendChatBlocking(question, history, engine, takeoverFrom, gen, controller.signal);
                return;
            }

            const reader = resp.body.getReader();
            const decoder = new TextDecoder();
            const splitter = createSseRecordSplitter();
            let sawEvent = false;
            let done = false;

            while (!done) {
                const { value, done: streamDone } = await reader.read();
                if (streamDone) break;
                const rawRecords = splitter.push(decoder.decode(value, { stream: true }));

                for (const rawRecord of rawRecords) {
                    const { event: evName, dataLines } = parseSseRecord(rawRecord);
                    if (dataLines.length === 0) continue;

                    let payload;
                    try {
                        payload = JSON.parse(dataLines.join('\n'));
                    } catch {
                        continue;
                    }
                    sawEvent = true;

                    if (evName === 'turn') {
                        if (gen === chatGen) chatStatus = tr('ai.chat_turn', { turn: payload.turn, max: payload.max });
                    } else if (evName === 'tool') {
                        if (gen === chatGen) chatStatus = tr('ai.chat_tool', { tool: payload.name, args: payload.args_preview || '' });
                    } else if (evName === 'tool_done') {
                        if (gen === chatGen) {
                            const secs = Number.isFinite(payload.ms) ? (payload.ms / 1000).toFixed(1) : null;
                            chatStatus = secs != null
                                ? tr('ai.chat_tool_done', { tool: payload.name || '', secs })
                                : tr('ai.chat_tool_done_notime', { tool: payload.name || '' });
                        }
                    } else if (evName === 'hedge_offer') {
                        if (gen === chatGen) {
                            chatOffer = { from: payload.from, to: payload.to };
                            chatStatus = tr('ai.chat_hedge_offer', { from: payload.from });
                        }
                    } else if (evName === 'fallback') {
                        if (gen === chatGen) {
                            chatOffer = null;
                            chatStatus = tr('ai.chat_fallback_status', { from: payload.from });
                        }
                    } else if (evName === 'hedge') {
                        if (gen === chatGen) chatStatus = tr('ai.chat_hedge', { from: payload.from });
                    } else if (evName === 'done') {
                        if (gen === chatGen) {
                            applyChatResult(payload);
                            chatOffer = null;
                        }
                        done = true;
                        break;
                    } else if (evName === 'error') {
                        throw new Error(payload.error || 'stream error');
                    }
                }
            }

            if (!done) {
                if (!sawEvent) {
                    await sendChatBlocking(question, history, engine, takeoverFrom, gen, controller.signal);
                } else {
                    throw new Error('stream ended without an answer');
                }
            }
        } catch (e) {
            if (e.name === 'AbortError') return; // superseded by a takeover — stay silent
            if (gen !== chatGen) return;
            toastStore.add(tr('ai.chat_send_failed', { error: e.message }), 'error');
            if (pushedQuestion) {
                chatMessages = chatMessages.slice(0, -1);
                chatInput = question;
                chatOffer = null;
            } else {
                chatOffer = takeoverOffer;
            }
        } finally {
            if (gen === chatGen) {
                chatSending = false;
                chatStatus = '';
            }
        }
    }

    async function sendChat() {
        const question = chatInput.trim();
        if (!question || chatSending) return;

        const history = chatMessages.map(m => ({ role: m.role, text: m.text }));
        chatMessages = [...chatMessages, { role: 'user', text: question }];
        chatInput = '';

        await runChat(question, history, chatEngine);
    }

    function takeoverGemini() {
        if (!chatOffer || !pendingChat) return;
        const offer = chatOffer;
        const { question, history } = pendingChat;
        if (chatController) chatController.abort();
        chatOffer = null;
        chatStatus = tr('ai.chat_hedge_switching');
        runChat(question, history, 'gemini', offer.from);
    }

    function onChatKeydown(e) {
        if (e.key === 'Enter' && !e.shiftKey) {
            e.preventDefault();
            sendChat();
        }
    }

    function clearChat() {
        if (chatController) chatController.abort();
        chatGen++; // invalidate any in-flight run
        chatSending = false;
        chatStatus = '';
        chatOffer = null;
        pendingChat = null;
        chatMessages = [];
        chatInput = '';
        chatSummary = null;
        chatSessionId = null;
        try {
            if (identityUser && typeof localStorage !== 'undefined') {
                if (context) {
                    // Per-entity conversation: clear its own keys. The session
                    // id is shared per-user, so leave it for the main chat.
                    localStorage.removeItem(chatStorageKey(HISTORY_BASE, identityUser));
                    localStorage.removeItem(chatStorageKey(SUMMARY_BASE, identityUser));
                } else {
                    clearChatConversationFor(identityUser);
                }
            }
        } catch (e) {
            console.error('chat history clear', e);
        }
    }
</script>

<div class="chat-panel" class:compact>
    <div class="chat-header">
        <h2>{$t('ai.chat_title')}</h2>
        <div class="chat-controls">
            <label class="engine-picker">
                {#if !compact}<span>{$t('ai.chat_engine_label')}</span>{/if}
                <select bind:value={chatEngine} on:change={saveChatEngine}>
                    <option value="glm">{$t('ai.chat_engine_glm')}</option>
                    <option value="gemini">{$t('ai.chat_engine_gemini')}</option>
                    <option value="deepseek">{$t('ai.chat_engine_deepseek')}</option>
                    <option value="mimo-pro">{$t('ai.chat_engine_mimo_pro')}</option>
                    <option value="mimo-flash">{$t('ai.chat_engine_mimo_flash')}</option>
                </select>
            </label>
            <button class="clear-btn" on:click={clearChat} disabled={chatMessages.length === 0}>
                {$t('ai.chat_clear')}
            </button>
        </div>
    </div>

    <div class="chat-log" bind:this={chatLogEl}>
        {#if chatMessages.length === 0}
            <div class="chat-empty">{$t('ai.chat_empty')}</div>
        {:else}
            {#each chatMessages as msg, i (i)}
                <div class="chat-bubble" class:user={msg.role === 'user'} class:assistant={msg.role === 'assistant'}>
                    {#if msg.hedge}
                        <div class="chat-hedge-note">{tr('ai.chat_hedge', { from: msg.hedge.from })}</div>
                    {/if}
                    {#if msg.fallback}
                        <div class="chat-hedge-note">{tr('ai.chat_fallback_note', { from: msg.fallback.from })}</div>
                    {/if}
                    <div class="chat-markdown">{@html renderChatText(msg.text)}</div>
                    {#if msg.role === 'assistant' && msg.model}
                        <div class="chat-model">
                            {msg.model}{#if cacheTag(msg.usage)} · <span class="chat-cache">{cacheTag(msg.usage)}</span>{/if}
                        </div>
                    {/if}
                </div>
            {/each}
        {/if}
        {#if chatSending}
            <div class="chat-bubble assistant chat-pending">
                <span class="spinner"></span>
                <span>{chatStatus || $t('ai.chat_sending')}</span>
                {#if chatOffer}
                    <button type="button" class="hedge-offer-btn" on:click={takeoverGemini}>
                        {$t('ai.chat_hedge_button')}
                    </button>
                {/if}
            </div>
        {/if}
    </div>

    <div class="chat-input-row">
        <textarea
            bind:value={chatInput}
            on:keydown={onChatKeydown}
            placeholder={$t('ai.chat_placeholder')}
            rows={compact ? 1 : 2}
            disabled={chatSending}
        ></textarea>
        <button
            class="chat-send-btn"
            on:click={sendChat}
            disabled={chatSending || !chatInput.trim()}
        >
            {chatSending ? $t('ai.chat_sending') : $t('ai.chat_send')}
        </button>
    </div>
</div>

<style>
    .chat-panel {
        display: flex;
        flex-direction: column;
        min-height: 0;
        min-width: 0;
        height: 100%;
        gap: 0.75rem;
    }

    .chat-header {
        display: flex;
        justify-content: space-between;
        align-items: center;
        padding: 0.25rem 0.75rem;
        min-width: 0;
        flex: 0 0 auto;
    }
    .chat-header h2 { margin: 0; color: #fff; font-size: 1.15rem; white-space: nowrap; }
    .chat-controls { display: flex; align-items: center; gap: 0.75rem; }
    .engine-picker {
        display: flex;
        align-items: center;
        gap: 0.4rem;
        color: #888;
        font-size: 0.85rem;
    }
    .engine-picker select {
        background: #121212;
        color: #fff;
        border: 1px solid #444;
        border-radius: 4px;
        padding: 0.3rem 0.5rem;
        font-size: 0.85rem;
    }
    .clear-btn {
        background: transparent;
        border: 1px solid #555;
        color: #aaa;
        border-radius: 4px;
        padding: 0.3rem 0.8rem;
        font-size: 0.85rem;
        cursor: pointer;
        transition: all 0.2s;
    }
    .clear-btn:hover:not(:disabled) { border-color: #ff6b6b; color: #ff6b6b; }
    .clear-btn:disabled { opacity: 0.4; cursor: not-allowed; }

    .chat-log {
        flex: 1 1 auto;
        display: flex;
        flex-direction: column;
        gap: 0.75rem;
        min-height: 0;
        min-width: 0;
        overflow-y: auto;
        padding: 0.25rem 0.75rem;
        background: #1e1e1e;
        border: 1px solid #333;
        border-radius: 8px;
    }
    .chat-empty { text-align: center; color: #666; padding: 2rem; }
    .chat-bubble {
        padding: 0.7rem 1rem;
        border-radius: 10px;
        font-size: 0.95rem;
        line-height: 1.45;
        min-width: 0;
    }
    /* Rendered markdown (renderChatText output, injected via {@html}). Svelte
       scoping can't reach these elements — every rule here is :global(). */
    .chat-markdown :global(p) { margin: 0 0 0.6rem; white-space: pre-wrap; word-break: break-word; }
    .chat-markdown :global(p:last-child) { margin-bottom: 0; }
    .chat-markdown :global(ul), .chat-markdown :global(ol) { margin: 0 0 0.6rem; padding-left: 1.4rem; }
    .chat-markdown :global(ul:last-child), .chat-markdown :global(ol:last-child) { margin-bottom: 0; }
    .chat-markdown :global(li) { margin: 0.15rem 0; }
    .chat-markdown :global(h1), .chat-markdown :global(h2), .chat-markdown :global(h3),
    .chat-markdown :global(h4), .chat-markdown :global(h5), .chat-markdown :global(h6) {
        margin: 0.6rem 0 0.4rem;
        color: #fff;
        line-height: 1.3;
    }
    .chat-markdown :global(h1):first-child, .chat-markdown :global(h2):first-child,
    .chat-markdown :global(h3):first-child { margin-top: 0; }
    .chat-markdown :global(h1) { font-size: 1.2rem; }
    .chat-markdown :global(h2) { font-size: 1.1rem; }
    .chat-markdown :global(h3), .chat-markdown :global(h4),
    .chat-markdown :global(h5), .chat-markdown :global(h6) { font-size: 1rem; }
    .chat-markdown :global(code) {
        background: #222;
        border: 1px solid #333;
        border-radius: 3px;
        padding: 0.05rem 0.3rem;
        font-size: 0.85em;
    }
    .chat-markdown :global(pre) {
        background: #111;
        border: 1px solid #2a2a2a;
        border-radius: 6px;
        padding: 0.6rem 0.75rem;
        overflow-x: auto;
        margin: 0 0 0.6rem;
    }
    .chat-markdown :global(pre:last-child) { margin-bottom: 0; }
    .chat-markdown :global(pre code) { background: none; border: none; padding: 0; font-size: 0.85rem; }
    .chat-markdown :global(table) {
        border-collapse: collapse;
        margin: 0 0 0.6rem;
        font-size: 0.9rem;
        width: 100%;
    }
    .chat-markdown :global(th), .chat-markdown :global(td) {
        border: 1px solid #333;
        padding: 0.3rem 0.5rem;
        text-align: left;
    }
    .chat-markdown :global(th) { background: #202020; color: #ccc; }
    .chat-markdown :global(a) { color: #6c9bd8; }
    .chat-markdown :global(a:hover) { text-decoration: underline; }

    .chat-bubble.user {
        align-self: flex-end;
        max-width: 66%;
        background: #2b2b2b;
        border: 1px solid #383838;
        color: #eee;
    }
    .chat-bubble.assistant {
        align-self: stretch;
        max-width: none;
        background: #161616;
        border: 1px solid #2a2a2a;
        color: #ddd;
    }
    .chat-model {
        margin-top: 0.35rem;
        font-size: 0.7rem;
        color: #777;
        text-transform: uppercase;
        letter-spacing: 0.4px;
    }
    .chat-cache { color: #4ade80; }
    .chat-hedge-note {
        margin-bottom: 0.35rem;
        font-size: 0.72rem;
        font-style: italic;
        color: #999;
    }
    .chat-pending {
        display: flex;
        align-items: center;
        gap: 0.5rem;
        color: #999;
    }
    .spinner {
        width: 0.9rem;
        height: 0.9rem;
        border: 2px solid #444;
        border-top-color: #4a69bd;
        border-radius: 50%;
        display: inline-block;
        animation: spin 0.7s linear infinite;
    }
    @keyframes spin { to { transform: rotate(360deg); } }
    .hedge-offer-btn {
        margin-left: auto;
        background: transparent;
        border: 1px solid #4a69bd;
        color: #8fa8f0;
        border-radius: 4px;
        padding: 0.2rem 0.6rem;
        font-size: 0.78rem;
        cursor: pointer;
        transition: all 0.2s;
        white-space: nowrap;
    }
    .hedge-offer-btn:hover { background: #4a69bd; color: #fff; }

    .chat-input-row {
        display: flex;
        gap: 0.75rem;
        align-items: flex-end;
        flex: 0 0 auto;
    }
    .chat-input-row textarea {
        flex: 1;
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
    .chat-input-row textarea:focus { border-color: #4a69bd; outline: none; }
    .chat-input-row textarea:disabled { opacity: 0.5; cursor: not-allowed; }

    .chat-send-btn {
        align-self: stretch;
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
    .chat-send-btn:hover:not(:disabled) { background: #218838; }
    .chat-send-btn:disabled { background: #333; color: #666; cursor: not-allowed; }

    /* ── Compact (embedded on an object page) ───────────────────────────────── */
    .chat-panel.compact { gap: 0.5rem; }
    .chat-panel.compact .chat-header { padding: 0 0.25rem; }
    .chat-panel.compact .chat-header h2 { font-size: 1rem; }
    .chat-panel.compact .chat-log { padding: 0.25rem 0.5rem; }
    .chat-panel.compact .chat-bubble { font-size: 0.88rem; padding: 0.55rem 0.8rem; }
    .chat-panel.compact .chat-input-row textarea { font-size: 0.9rem; padding: 0.6rem; }
    .chat-panel.compact .chat-send-btn { padding: 0.6rem 1.2rem; font-size: 0.88rem; }
</style>
