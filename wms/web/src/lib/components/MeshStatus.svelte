<script>
    import { onMount, onDestroy } from 'svelte';
    import { base } from '$app/paths';

    let meshNodes = [];
    let selfStatus = null;
    let loading = true;
    let pollInterval;

    async function fetchMeshNodes() {
        try {
            const [nodesRes, statusRes] = await Promise.all([
                fetch('/api/mesh/nodes'),
                fetch('/api/mesh/status')
            ]);
            if (nodesRes.ok) meshNodes = await nodesRes.json();
            if (statusRes.ok) selfStatus = await statusRes.json();
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

    function getNodeLabel(node) {
        const instanceId = node.instance_id;
        const role = node.role.toUpperCase();

        let identifier = '';

        // If node has a non-localhost URL, use domain as identifier
        if (node.base_url && !node.base_url.includes('localhost')) {
            try {
                const url = new URL(node.base_url);
                identifier = url.hostname; // e.g., "pda.repair"
            } catch (e) {
                // URL parsing failed, fall through to hash extraction
            }
        }

        // If no domain available, extract meaningful part from instance_id
        if (!identifier) {
            let hash = instanceId;

            // Remove common prefixes
            hash = hash.replace(/^production_/, '');
            hash = hash.replace(/^local_/, '');
            hash = hash.replace(/^instance_/, '');

            // For UUIDs, take first two segments
            if (hash.includes('-')) {
                const uuidParts = hash.split('-');
                if (uuidParts.length >= 5) {
                    hash = uuidParts.slice(0, 2).join('-');
                }
            }

            // Limit length if still too long
            if (hash.length > 20) {
                hash = hash.substring(0, 20);
            }

            identifier = hash;
        }

        return `${role}-${identifier}`;
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
                <span class="node-label" title="ID: {selfStatus.instance_id}">{selfStatus.instance_name || selfStatus.instance_id.substring(0, 8)}</span>
                <span class="node-status online"></span>
            </div>
        {/if}
        {#if meshNodes.length === 0}
            <div class="mesh-node offline">
                <span class="node-icon">⚠️</span>
                <span class="node-label">No peers</span>
            </div>
        {:else}
            {#each meshNodes as node}
                <div class="mesh-node" class:online={node.status === 'active'} class:degraded={node.status === 'degraded'} class:offline={node.status === 'offline'}>
                    <span class="node-icon">{getNodeIcon(node.role)}</span>
                    <span class="node-label">{getNodeLabel(node)}</span>
                    <span class="node-status" class:online={node.status === 'active'} class:degraded={node.status === 'degraded'}></span>
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
