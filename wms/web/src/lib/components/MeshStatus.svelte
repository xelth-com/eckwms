<script>
    import { onMount, onDestroy } from 'svelte';
    import { base } from '$app/paths';
    import { get } from 'svelte/store';
    import { authStore } from '$lib/stores/authStore';
    import { t, tr } from '$lib/i18n';

    let meshNodes = [];
    let selfStatus = null;
    let relayStatus = 'unknown'; // 'online' | 'offline' | 'unknown'
    let loading = true;
    let pollInterval;
    let aiUsage = null; // { used_24h, calls_24h, promo_cap }
    let boardStatus = 'unknown'; // 'online' | 'offline' | 'unknown'
    let boardUrl = 'https://9eck.com';

    async function fetchAiUsage() {
        try {
            const res = await fetch('/api/ai/usage', { headers: authHeaders() });
            if (res.ok) aiUsage = await res.json();
        } catch (_) { /* non-fatal */ }
    }

    // Compact token count: 12345 → "12.3k", 1500000 → "1.5M".
    function fmtTokens(n) {
        if (n == null) return '–';
        if (n >= 1_000_000) return (n / 1_000_000).toFixed(1) + 'M';
        if (n >= 1_000) return (n / 1_000).toFixed(n >= 100_000 ? 0 : 1) + 'k';
        return String(n);
    }
    // RFC3339 → "30.07" (day.month).
    function fmtDate(s) {
        if (!s) return '';
        const d = new Date(s);
        if (isNaN(d.getTime())) return '';
        return d.toLocaleDateString('ru-RU', { day: '2-digit', month: '2-digit' });
    }
    // Build the AI-budget line: colour class + label + tooltip. Headline is the
    // "will the budget last to the period reset?" forecast.
    function aiLine(u) {
        if (!u) return null;
        if (u.source === 'authority') {
            // v3 plan-engine buckets — absent/0 from a v2 authority, so these
            // suffixes simply don't render until the v3 engine is deployed.
            const hasExtra = (u.extra_balance ?? 0) > 0;
            const extraTip = hasExtra
                ? ' ' + tr('mesh.extra_pack_tip', { extra: fmtTokens(u.extra_balance) })
                : '';
            // "12k+30k / 500k" — monthly remainder + extra pack.
            const rem = fmtTokens(u.tokens_remaining) + (hasExtra ? '+' + fmtTokens(u.extra_balance) : '');
            const grant = fmtTokens(u.monthly_grant);
            const rate = fmtTokens(u.burn_per_day) + tr('mesh.per_day_suffix');
            const reset = fmtDate(u.period_ends_at);
            if (u.loan_outstanding > 0) {
                const creditTip = (u.credit_limit ?? 0) > 0
                    ? ' ' + tr('mesh.credit_of_limit_tip', {
                        used: fmtTokens(u.loan_outstanding),
                        limit: fmtTokens(u.credit_limit)
                    })
                    : '';
                return {
                    cls: 'over',
                    label: tr('mesh.ai_loan_label', { rem, grant }),
                    tip: tr('mesh.ai_loan_tip', { loan: fmtTokens(u.loan_outstanding), reset, rate }) + creditTip + extraTip
                };
            }
            if (u.will_last === false) {
                const empty = fmtDate(u.est_empty_at);
                const emptyStr = empty ? '~' + empty : tr('mesh.until_reset');
                const days = u.est_days_left >= 0 ? tr('mesh.days_left_paren', { n: u.est_days_left }) : '';
                return {
                    cls: 'warn',
                    label: tr('mesh.ai_warn_label', { rem, grant }),
                    tip: tr('mesh.ai_warn_tip', { empty: emptyStr, rate, reset, days }) + extraTip
                };
            }
            const days = u.est_days_left >= 0 ? tr('mesh.days_left_suffix', { n: u.est_days_left }) : '';
            return {
                cls: 'ok',
                label: tr('mesh.ai_ok_label', { rem, grant }),
                tip: tr('mesh.ai_ok_tip', { reset, rate, days }) + extraTip
            };
        }
        // Local fallback (studio mode / before the first managed mint).
        const r = u.promo_cap ? u.used_24h / u.promo_cap : 0;
        return {
            cls: r >= 1 ? 'over' : r >= 0.7 ? 'warn' : 'ok',
            label: tr('mesh.ai_local_label', { used: fmtTokens(u.used_24h), cap: fmtTokens(u.promo_cap) }),
            tip: tr('mesh.ai_local_tip', { calls: u.calls_24h })
        };
    }

    $: budgetLine = aiLine(aiUsage);

    function authHeaders() {
        const state = get(authStore);
        return state.token ? { 'Authorization': `Bearer ${state.token}` } : {};
    }

    async function fetchMeshNodes() {
        try {
            const headers = authHeaders();
            const [nodesRes, statusRes] = await Promise.all([
                fetch('/api/mesh/nodes', { headers }),
                fetch('/api/mesh/status', { headers })
            ]);
            if (statusRes.ok) selfStatus = await statusRes.json();
            if (nodesRes.ok) {
                const body = await nodesRes.json();
                // Backend now returns { relay, nodes }; tolerate old array shape too
                const allNodes = Array.isArray(body) ? body : (body.nodes || []);
                relayStatus = Array.isArray(body) ? 'online' : (body.relay || 'unknown');
                if (!Array.isArray(body)) {
                    boardStatus = body.board || 'unknown';
                    if (body.board_url) boardUrl = body.board_url;
                }
                // Filter out self from peers list (shown separately)
                meshNodes = selfStatus
                    ? allNodes.filter(n => n.instance_id !== selfStatus.instance_id)
                    : allNodes;
            }
            loading = false;
        } catch (error) {
            console.error('Failed to fetch mesh nodes:', error);
            loading = false;
        }
    }

    onMount(() => {
        fetchMeshNodes();
        fetchAiUsage();
        // Poll every 30 seconds
        pollInterval = setInterval(() => { fetchMeshNodes(); fetchAiUsage(); }, 30000);
    });

    onDestroy(() => {
        if (pollInterval) clearInterval(pollInterval);
    });

    function getNodeIcon(role) {
        switch (role) {
            case 'master': return '👑';
            case 'peer': return '🖥️';
            case 'edge': return '📱';
            default: return '🔗';
        }
    }

    // Row icon: the bulletin board gets the pin, relay-cluster nodes the
    // antenna; everything else falls back to the role icon.
    function iconFor(node) {
        const kind = nodeKind(node.base_url, node.role);
        if (kind === 'BOARD') return '📌';
        if (kind === 'RELAY') return '📡';
        return getNodeIcon(node.role);
    }

    // True if `host` is a bare IP literal (IPv4 or IPv6) — i.e. NOT a domain.
    function isIpHost(host) {
        if (!host) return false;
        if (/^\d{1,3}(\.\d{1,3}){3}$/.test(host)) return true; // IPv4
        if (host.includes(':')) return true;                   // IPv6
        return false;
    }

    // The node's real domain, or '' when it has none (localhost / bare IP / no url).
    function domainOf(baseUrl) {
        if (!baseUrl) return '';
        try {
            const host = new URL(baseUrl).hostname;
            if (!host || host === 'localhost' || isIpHost(host)) return '';
            return host; // e.g. "pda.repair"
        } catch (e) {
            return '';
        }
    }

    // Short, stable fallback when there's no domain: first UUID segment
    // (e.g. "00000000"), with legacy prefixes stripped.
    function shortId(instanceId) {
        let h = (instanceId || '')
            .replace(/^production_/, '')
            .replace(/^local_/, '')
            .replace(/^instance_/, '');
        if (h.includes('-')) h = h.split('-')[0];
        return h.length > 20 ? h.substring(0, 20) : h;
    }

    // Display name per the rule: domain if the node has one, else its UUID.
    function nodeName(baseUrl, instanceId) {
        return domainOf(baseUrl) || shortId(instanceId);
    }

    // Display role (owner naming 2026-07-17): MASTER only when the node
    // actually announces the master role; `9eck.com` specifically is the
    // public BULLETIN BOARD; other domain-advertising cluster nodes are the
    // RELAY tier; bare-LAN data nodes stay HOME. (Display only — no stored
    // role/data changes.)
    function nodeKind(baseUrl, role) {
        if (role === 'master') return 'MASTER';
        // Which domain is the public bulletin BOARD is a deployment choice.
        // Build-time env VITE_BOARD_DOMAIN overrides; default keeps today's host.
        const boardDomain = import.meta.env.VITE_BOARD_DOMAIN || '9eck.com';
        const domain = domainOf(baseUrl);
        if (domain === boardDomain) return 'BOARD';
        if (domain) return 'RELAY';
        return 'HOME';
    }

    function getNodeLabel(node) {
        return `${nodeKind(node.base_url, node.role)}-${nodeName(node.base_url, node.instance_id)}`;
    }

    // Vertical order (owner, 2026-07-17), top → bottom: SELF (rendered above
    // this list), then regular data nodes, then the RELAY tier, and the
    // bulletin BOARD at the very bottom. Ties sort by display name.
    function kindRank(node) {
        switch (nodeKind(node.base_url, node.role)) {
            case 'BOARD': return 3;
            case 'RELAY': return 2;
            default: return 1;
        }
    }
    $: sortedNodes = [...meshNodes].sort((a, b) =>
        kindRank(a) - kindRank(b) ||
        nodeName(a.base_url, a.instance_id).localeCompare(nodeName(b.base_url, b.instance_id)));

    // Display domain for the board chip: hostname of boardUrl, falling back
    // to the default board domain if the URL is unparseable.
    $: boardDomain = (() => {
        try {
            return new URL(boardUrl).hostname || '9eck.com';
        } catch (e) {
            return '9eck.com';
        }
    })();

    // The status dot's COLOUR = the node's status (🟢 online / 🟡 degraded /
    // 🔴 offline) — the stable, meaningful signal. Its tooltip shows how long
    // since the relay last heard from the node (nodes heartbeat every ~5 min, so
    // a healthy node's "last seen" is just where in that cycle we sampled; only
    // a node that's actually down drifts far past 5 min).

    // Age in ms since `last_seen` (null when missing/unparseable).
    function ageMs(lastSeen) {
        if (!lastSeen) return null;
        const t = Date.parse(lastSeen);
        if (isNaN(t)) return null;
        return Math.max(0, Date.now() - t);
    }

    // Human-readable "last seen" for the dot tooltip.
    function fmtAge(lastSeen) {
        const a = ageMs(lastSeen);
        if (a === null) return tr('mesh.last_seen_unknown');
        const s = Math.round(a / 1000);
        if (s < 60) return tr('mesh.last_seen_s', { n: s });
        const m = Math.round(s / 60);
        if (m < 60) return tr('mesh.last_seen_m', { n: m });
        const h = Math.round(m / 60);
        if (h < 24) return tr('mesh.last_seen_h', { n: h });
        return tr('mesh.last_seen_d', { n: Math.round(h / 24) });
    }
</script>

<div class="mesh-status">
    {#if loading}
        <div class="mesh-node loading">
            <span class="node-icon">⏳</span>
            <span class="node-label">{$t('mesh.loading')}</span>
        </div>
    {:else}
        {#if budgetLine}
            <div class="mesh-node ai-budget" title={budgetLine.tip}>
                <span class="node-icon">🤖</span>
                <span class="node-label">{budgetLine.label}</span>
                <span class="ai-dot {budgetLine.cls}"></span>
            </div>
        {/if}
        {#if selfStatus}
            <div class="mesh-node self">
                <!-- The house IS the "this node" marker; when this node truly
                     announces the master role the label says MASTER instead of
                     SELF (house + SELF would be redundant there). -->
                <span class="node-icon">🏠</span>
                <span class="node-label" title={$t('mesh.self_id_tip', { id: selfStatus.instance_id })}>{selfStatus.role === 'master' ? 'MASTER' : 'SELF'}-{nodeName(selfStatus.base_url, selfStatus.instance_id)}</span>
                <span class="node-status online" title={$t('mesh.self_live_tip')}></span>
            </div>
        {/if}
        {#if relayStatus === 'offline'}
            <div class="mesh-node offline" title={$t('mesh.relay_offline_tip')}>
                <span class="node-icon">📡</span>
                <span class="node-label">{$t('mesh.relay_offline')}</span>
            </div>
        {:else if meshNodes.length === 0}
            <div class="mesh-node offline">
                <span class="node-icon">⚠️</span>
                <span class="node-label">{$t('mesh.no_peers')}</span>
            </div>
        {:else}
            {#each sortedNodes as node}
                <div class="mesh-node" class:online={node.status === 'online' || node.status === 'active'} class:degraded={node.status === 'degraded'} class:offline={node.status === 'offline'}>
                    <span class="node-icon">{iconFor(node)}</span>
                    <span class="node-label">{getNodeLabel(node)}</span>
                    <span class="node-status" class:online={node.status === 'online' || node.status === 'active'} class:degraded={node.status === 'degraded'} class:offline={node.status === 'offline'} title={fmtAge(node.last_seen)}></span>
                </div>
            {/each}
        {/if}
        {#if boardStatus !== 'unknown'}
            <div class="mesh-node" class:online={boardStatus === 'online'} class:offline={boardStatus === 'offline'}
                 title={boardStatus === 'online' ? $t('mesh.board_online_tip') : $t('mesh.board_offline_tip')}>
                <span class="node-icon">📌</span>
                <span class="node-label">BOARD-{boardDomain}</span>
                <span class="node-status" class:online={boardStatus === 'online'} class:offline={boardStatus === 'offline'}></span>
            </div>
        {/if}
    {/if}
</div>

<style>
    .mesh-status {
        display: flex;
        flex-direction: column;
        gap: 4px;
        font-size: 0.7rem;
    }

    .mesh-node {
        display: flex;
        align-items: center;
        gap: 6px;
        padding: 4px 8px;
        border-radius: 4px;
        background: #1a1a1a;
        border: 1px solid #333;
        transition: all 0.2s;
    }

    .mesh-node.self {
        background: rgba(74, 105, 189, 0.15);
        border-color: rgba(74, 105, 189, 0.4);
    }

    .mesh-node.self .node-label {
        color: #7b9ff0;
    }

    .mesh-node.online {
        background: rgba(40, 167, 69, 0.1);
        border-color: rgba(40, 167, 69, 0.3);
    }

    .mesh-node.degraded {
        background: rgba(255, 193, 7, 0.1);
        border-color: rgba(255, 193, 7, 0.3);
    }

    .mesh-node.offline {
        background: rgba(220, 53, 69, 0.1);
        border-color: rgba(220, 53, 69, 0.3);
        opacity: 0.7;
    }

    .mesh-node.loading {
        background: rgba(255, 193, 7, 0.1);
        border-color: rgba(255, 193, 7, 0.3);
    }

    .node-icon {
        font-size: 1rem;
        line-height: 1;
    }

    .node-label {
        flex: 1;
        font-weight: 600;
        color: #ccc;
        white-space: nowrap;
        overflow: hidden;
        text-overflow: ellipsis;
    }

    .mesh-node.online .node-label {
        color: #28a745;
    }

    .mesh-node.degraded .node-label {
        color: #ffc107;
    }

    .mesh-node.offline .node-label {
        color: #dc3545;
    }

    .node-status {
        width: 6px;
        height: 6px;
        border-radius: 50%;
        background: #666;
    }

    .node-status.online {
        background: #28a745;
        box-shadow: 0 0 6px rgba(40, 167, 69, 0.6);
    }

    .node-status.degraded {
        background: #ffc107;
        box-shadow: 0 0 6px rgba(255, 193, 7, 0.6);
    }

    .node-status.offline {
        background: #dc3545;
        box-shadow: 0 0 6px rgba(220, 53, 69, 0.6);
    }

    .mesh-node.ai-budget .node-label { color: #9aa4b2; }
    .ai-dot {
        width: 6px;
        height: 6px;
        border-radius: 50%;
        background: #666;
        margin-left: auto;
    }
    .ai-dot.ok { background: #28a745; box-shadow: 0 0 6px rgba(40,167,69,0.6); }
    .ai-dot.warn { background: #ffc107; box-shadow: 0 0 6px rgba(255,193,7,0.6); }
    .ai-dot.over { background: #dc3545; box-shadow: 0 0 6px rgba(220,53,69,0.6); }
</style>
