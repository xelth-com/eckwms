<script>
    import { onMount, onDestroy } from 'svelte';
    import { base } from '$app/paths';
    import { get } from 'svelte/store';
    import { authStore } from '$lib/stores/authStore';

    let meshNodes = [];
    let selfStatus = null;
    let relayStatus = 'unknown'; // 'online' | 'offline' | 'unknown'
    let loading = true;
    let pollInterval;

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
        // Poll every 30 seconds
        pollInterval = setInterval(fetchMeshNodes, 30000);
    });

    onDestroy(() => {
        if (pollInterval) clearInterval(pollInterval);
    });

    function getNodeIcon(role) {
        switch (role) {
            case 'master': return '🌐';
            case 'peer': return '🖥️';
            case 'edge': return '📱';
            default: return '🔗';
        }
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
    // (e.g. "de1911de"), with legacy prefixes stripped.
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

    function getNodeLabel(node) {
        return `${node.role.toUpperCase()}-${nodeName(node.base_url, node.instance_id)}`;
    }
</script>

<div class="mesh-status">
    {#if loading}
        <div class="mesh-node loading">
            <span class="node-icon">⏳</span>
            <span class="node-label">Loading...</span>
        </div>
    {:else}
        {#if selfStatus}
            <div class="mesh-node self">
                <span class="node-icon">🏠</span>
                <span class="node-label" title="ID: {selfStatus.instance_id}">{nodeName(selfStatus.base_url, selfStatus.instance_id)}</span>
                <span class="node-status online"></span>
            </div>
        {/if}
        {#if relayStatus === 'offline'}
            <div class="mesh-node offline" title="Central tracker (relay) unreachable — peer discovery paused">
                <span class="node-icon">📡</span>
                <span class="node-label">Relay offline</span>
            </div>
        {:else if meshNodes.length === 0}
            <div class="mesh-node offline">
                <span class="node-icon">⚠️</span>
                <span class="node-label">No peers</span>
            </div>
        {:else}
            {#each meshNodes as node}
                <div class="mesh-node" class:online={node.status === 'online' || node.status === 'active'} class:degraded={node.status === 'degraded'} class:offline={node.status === 'offline'}>
                    <span class="node-icon">{getNodeIcon(node.role)}</span>
                    <span class="node-label">{getNodeLabel(node)}</span>
                    <span class="node-status" class:online={node.status === 'online' || node.status === 'active'} class:degraded={node.status === 'degraded'}></span>
                </div>
            {/each}
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
</style>
