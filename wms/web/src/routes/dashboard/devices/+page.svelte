<script>
    import { onMount } from 'svelte';
    import { api } from '$lib/api';
    import { toastStore } from '$lib/stores/toastStore';
    import { base } from '$app/paths';

    // Tabs
    let activeTab = 'scanners'; // 'scanners' | 'servers'

    // Scanners Data
    let devices = [];
    let meshNodes = [];
    let loading = true;
    let qrUrl = '';
    let showQr = false;
    let qrType = 'standard';

    // Server Pairing Data
    let pairingStep = 'idle'; // idle, hosting, connecting, waiting_approval, approving
    let pairingCode = '';
    let connectCode = '';
    let pendingPeer = null;
    let pollTimer = null;

    // --- SCANNERS LOGIC ---

    async function loadScannersData() {
        loading = true;
        try {
            const [devicesData, nodesData, statusData] = await Promise.all([
                api.get('/api/admin/devices?include_deleted=true'),
                api.get('/api/mesh/nodes'),
                api.get('/api/mesh/status')
            ]);
            devices = devicesData || [];

            let nodes = nodesData || [];
            if (statusData && statusData.instance_id) {
                const selfNode = {
                    instance_id: statusData.instance_id,
                    role: statusData.role || 'peer',
                    base_url: statusData.base_url || 'http://localhost:3210',
                    is_self: true
                };
                nodes = [selfNode, ...nodes];
            }
            meshNodes = nodes;
        } catch (e) {
            toastStore.add('Failed to load devices: ' + e.message, 'error');
        } finally {
            loading = false;
        }
    }

    async function updateStatus(deviceId, status) {
        try {
            await api.put(`/api/admin/devices/${deviceId}/status`, { status });
            toastStore.add(`Device ${status}`, 'success');
            devices = devices.map(d => d.deviceId === deviceId ? { ...d, status } : d);
        } catch (e) {
            toastStore.add(e.message, 'error');
        }
    }

    async function updateHomeNode(deviceId, homeInstanceId) {
        try {
            await api.put(`/api/admin/devices/${deviceId}/home`, { homeInstanceId });
            toastStore.add('Home Node updated', 'success');
        } catch (e) {
            toastStore.add(e.message, 'error');
        }
    }

    async function deleteDevice(deviceId) {
        if (!confirm('Delete this device?')) return;
        try {
            await api.delete(`/api/admin/devices/${deviceId}`);
            toastStore.add('Device deleted', 'success');
            await loadScannersData();
        } catch (e) {
            toastStore.add(e.message, 'error');
        }
    }

    async function restoreDevice(deviceId) {
        try {
            await api.post(`/api/admin/devices/${deviceId}/restore`);
            toastStore.add('Device restored', 'success');
            await loadScannersData();
        } catch (e) {
            toastStore.add(e.message, 'error');
        }
    }

    async function loadQr(type = 'standard') {
        if (showQr && qrType === type) { showQr = false; return; }
        qrType = type;
        try {
            const token = localStorage.getItem('auth_token');
            const url = type === 'vip'
                ? '/api/internal/pairing-qr?type=vip'
                : '/api/internal/pairing-qr';
            const res = await fetch(url, { headers: { Authorization: `Bearer ${token}` } });
            const blob = await res.blob();
            qrUrl = URL.createObjectURL(blob);
            showQr = true;
        } catch (e) {
            toastStore.add('Failed to load QR', 'error');
        }
    }

    function getNodeName(instanceId) {
        if (!instanceId) return 'Unknown';
        const node = meshNodes.find(n => n.instance_id === instanceId);
        let role = node ? node.role.toUpperCase() : 'PEER';
        let identifier = instanceId;
        if (node && node.base_url && !node.base_url.includes('localhost')) {
            try { identifier = new URL(node.base_url).hostname; } catch (e) { /* ignore */ }
        }
        if (identifier.length > 20) identifier = identifier.substring(0, 20);
        return `${role}-${identifier}`;
    }

    // --- SERVER PAIRING LOGIC ---

    let serverNodes = [];
    let selfInfo = null;

    async function loadServersData() {
        loading = true;
        try {
            const [nodes, status] = await Promise.all([
                api.get('/api/mesh/nodes'),
                api.get('/api/mesh/status')
            ]);
            serverNodes = nodes || [];
            selfInfo = status || null;
        } catch (e) {
            toastStore.add('Failed to load nodes', 'error');
        } finally {
            loading = false;
        }
    }

    async function deleteServer(id) {
        if (!confirm('Unpair this server?')) return;
        try {
            await api.delete(`/api/admin/mesh/${id}`);
            toastStore.add('Server unpaired', 'success');
            await loadServersData();
        } catch (e) {
            toastStore.add(e.message, 'error');
        }
    }

    // Host Flow
    async function startHosting() {
        try {
            const res = await api.post('/api/pairing/host', {});
            pairingCode = res.code;
            pairingStep = 'hosting';
            pollForPeer(res.code);
        } catch (e) {
            toastStore.add(e.message, 'error');
        }
    }

    async function pollForPeer(code) {
        if (pairingStep !== 'hosting') return;
        try {
            const res = await api.post('/api/pairing/check', { code });
            if (res.found) {
                pendingPeer = { id: res.remote_instance_id, name: res.remote_instance_name };
                pairingStep = 'approving';
            } else {
                pollTimer = setTimeout(() => pollForPeer(code), 2000);
            }
        } catch (e) {
            console.error('Polling error', e);
        }
    }

    async function approvePeer() {
        try {
            await api.post('/api/pairing/approve', {
                code: pairingCode,
                remote_instance_id: pendingPeer.id
            });
            toastStore.add('Pairing successful!', 'success');
            pairingStep = 'idle';
            loadServersData();
        } catch (e) {
            toastStore.add(e.message, 'error');
        }
    }

    // Client Flow
    async function connectToServer() {
        if (!connectCode) return;
        try {
            pairingStep = 'connecting';
            await api.post('/api/pairing/connect', { code: connectCode });
            toastStore.add('Connected! Waiting for approval...', 'info');
            pairingStep = 'waiting_approval';
            pollForApproval(connectCode);
        } catch (e) {
            pairingStep = 'idle';
            toastStore.add(e.message, 'error');
        }
    }

    async function pollForApproval(code) {
        if (pairingStep !== 'waiting_approval') return;
        try {
            const res = await api.post('/api/pairing/finalize', { code });
            if (res.status === 'finalized') {
                if (res.network_key) {
                    await saveConfig(res.network_key);
                }
                toastStore.add('Pairing Complete! Server saved.', 'success');
                pairingStep = 'idle';
                loadServersData();
            } else {
                pollTimer = setTimeout(() => pollForApproval(code), 2000);
            }
        } catch (e) {
            console.error(e);
        }
    }

    async function saveConfig(key) {
        try {
            await api.post('/api/admin/config/save-key', { network_key: key });
            toastStore.add('Network Key saved. Restart server to apply.', 'warning');
        } catch (e) {
            toastStore.add('Failed to save config: ' + e.message, 'error');
        }
    }

    function cancelPairing() {
        if (pollTimer) clearTimeout(pollTimer);
        pairingStep = 'idle';
        pairingCode = '';
        connectCode = '';
        pendingPeer = null;
    }

    function switchTab(tab) {
        activeTab = tab;
        if (tab === 'scanners') loadScannersData();
        else loadServersData();
    }

    onMount(() => {
        loadScannersData();
    });
</script>

<div class="page">
    <header>
        <h1>Connectivity & Devices</h1>
        <div class="tabs">
            <button class="tab" class:active={activeTab === 'scanners'} on:click={() => switchTab('scanners')}>
                Scanners (PDAs)
            </button>
            <button class="tab" class:active={activeTab === 'servers'} on:click={() => switchTab('servers')}>
                Mesh Servers
            </button>
        </div>
    </header>

    {#if activeTab === 'scanners'}
        <!-- SCANNERS VIEW -->
        <div class="toolbar">
            <div class="action-group">
                <button class="btn secondary" class:active={showQr && qrType === 'standard'} on:click={() => loadQr('standard')}>
                    Standard QR
                </button>
                <button class="btn primary" class:active={showQr && qrType === 'vip'} on:click={() => loadQr('vip')}>
                    Auto-Approve QR
                </button>
            </div>
            <button class="btn secondary" on:click={loadScannersData}>Refresh</button>
        </div>

        {#if showQr && qrUrl}
            <div class="qr-panel" class:vip={qrType === 'vip'}>
                <h3>{qrType === 'vip' ? 'Auto-Approve Pairing' : 'Standard Pairing'}</h3>
                <img src={qrUrl} alt="Pairing QR" />
                <p class="hint">
                    {#if qrType === 'vip'}
                        <strong>Warning:</strong> Devices scanning this code will be <u>immediately authorized</u>.
                    {:else}
                        Devices scanning this code will appear as <strong>Pending</strong> below.
                    {/if}
                </p>
                <button class="btn-text" on:click={() => showQr = false}>Close</button>
            </div>
        {/if}

        <div class="list-container">
            {#if loading}
                <div class="loading">Loading devices...</div>
            {:else if devices.length === 0}
                <div class="empty">No devices registered. Scan a QR code to add one.</div>
            {:else}
                <table>
                    <thead>
                        <tr>
                            <th>Status</th>
                            <th>Device Name</th>
                            <th>ID / Key</th>
                            <th>Home Node</th>
                            <th>Last Seen</th>
                            <th>Actions</th>
                        </tr>
                    </thead>
                    <tbody>
                        {#each devices as device}
                            <tr class:deleted={device.deletedAt}>
                                <td>
                                    {#if device.deletedAt}
                                        <span class="badge deleted">Deleted</span>
                                    {:else}
                                        <span class="badge {device.status}">{device.status}</span>
                                    {/if}
                                </td>
                                <td>{device.name || 'Unknown'}</td>
                                <td>
                                    <div class="mono-id" title={device.deviceId}>{device.deviceId.substring(0, 8)}...</div>
                                    <div class="mono-key">{device.publicKey ? device.publicKey.substring(0, 8) + '...' : '-'}</div>
                                </td>
                                <td>
                                    <select
                                        value={device.homeInstanceId}
                                        on:change={(e) => updateHomeNode(device.deviceId, e.target.value)}
                                        disabled={!!device.deletedAt}
                                        class="node-select"
                                    >
                                        <option value={device.homeInstanceId}>{getNodeName(device.homeInstanceId)} (Current)</option>
                                        {#each meshNodes as node}
                                            {#if node.instance_id !== device.homeInstanceId}
                                                <option value={node.instance_id}>{getNodeName(node.instance_id)}</option>
                                            {/if}
                                        {/each}
                                    </select>
                                </td>
                                <td>{new Date(device.lastSeenAt).toLocaleString()}</td>
                                <td class="actions">
                                    {#if device.deletedAt}
                                        <button class="btn-icon" title="Restore" on:click={() => restoreDevice(device.deviceId)}>&#9851;</button>
                                    {:else}
                                        {#if device.status === 'pending' || device.status === 'blocked'}
                                            <button class="btn-icon approve" title="Approve" on:click={() => updateStatus(device.deviceId, 'active')}>&#10003;</button>
                                        {/if}
                                        {#if device.status === 'active' || device.status === 'pending'}
                                            <button class="btn-icon block" title="Block" on:click={() => updateStatus(device.deviceId, 'blocked')}>&#10007;</button>
                                        {/if}
                                        <button class="btn-icon delete" title="Delete" on:click={() => deleteDevice(device.deviceId)}>&#128465;</button>
                                    {/if}
                                </td>
                            </tr>
                        {/each}
                    </tbody>
                </table>
            {/if}
        </div>

    {:else}
        <!-- SERVERS VIEW -->
        <div class="toolbar">
            <div class="action-group">
                <button class="btn primary" on:click={startHosting}>+ Invite Server</button>
            </div>
            <div class="join-group">
                <input type="text" placeholder="XXX-XXX" bind:value={connectCode} maxlength="7" />
                <button class="btn secondary" on:click={connectToServer} disabled={connectCode.length < 6}>Join Network</button>
            </div>
        </div>

        {#if selfInfo}
            <div class="identity-card">
                <h3>This Server</h3>
                <div class="identity-row">
                    <span class="identity-label">Name</span>
                    <span class="identity-value">{selfInfo.instance_name || '—'}</span>
                </div>
                <div class="identity-row">
                    <span class="identity-label">Instance ID</span>
                    <span class="identity-value mono">{selfInfo.instance_id}</span>
                </div>
                <div class="identity-row">
                    <span class="identity-label">Mesh ID</span>
                    <span class="identity-value mono">{selfInfo.mesh_id}</span>
                </div>
                <div class="identity-row">
                    <span class="identity-label">Base URL</span>
                    <span class="identity-value mono">{selfInfo.base_url || 'not set'}</span>
                </div>
            </div>
        {/if}

        {#if pairingStep !== 'idle'}
            <div class="pairing-modal">
                <div class="modal-content">
                    {#if pairingStep === 'hosting'}
                        <h3>Waiting for connection...</h3>
                        <div class="big-code">{pairingCode}</div>
                        <p>Enter this code on the other server</p>
                        <div class="spinner"></div>
                        <button class="btn secondary" on:click={cancelPairing}>Cancel</button>

                    {:else if pairingStep === 'approving'}
                        <h3>Connection Request</h3>
                        <p>Server <strong>{pendingPeer?.name || 'Unknown'}</strong> wants to connect.</p>
                        <p class="mono-sm">ID: {pendingPeer?.id}</p>
                        <div class="modal-actions">
                            <button class="btn secondary" on:click={cancelPairing}>Deny</button>
                            <button class="btn primary" on:click={approvePeer}>Approve</button>
                        </div>

                    {:else if pairingStep === 'connecting'}
                        <h3>Connecting...</h3>
                        <div class="spinner"></div>

                    {:else if pairingStep === 'waiting_approval'}
                        <h3>Waiting for approval...</h3>
                        <p>Please approve the request on the Host server.</p>
                        <div class="spinner"></div>
                        <button class="btn secondary" on:click={cancelPairing}>Cancel</button>
                    {/if}
                </div>
            </div>
        {/if}

        <div class="list-container">
            {#if loading}
                <div class="loading">Loading nodes...</div>
            {:else if serverNodes.length === 0}
                <div class="empty">No paired servers. Use "Invite Server" or enter a code to join a network.</div>
            {:else}
                <table>
                    <thead>
                        <tr>
                            <th>Status</th>
                            <th>Name</th>
                            <th>Role</th>
                            <th>Address</th>
                            <th>Actions</th>
                        </tr>
                    </thead>
                    <tbody>
                        {#each serverNodes as node}
                            <tr>
                                <td>
                                    <span class="dot" class:online={node.status === 'active'} class:degraded={node.status === 'degraded'}></span>
                                    {node.status === 'active' ? 'Online' : node.status === 'degraded' ? 'Unstable' : 'Offline'}
                                </td>
                                <td>{node.name}</td>
                                <td><span class="role-badge {node.role}">{node.role}</span></td>
                                <td class="mono">{node.base_url || 'Relay Only'}</td>
                                <td>
                                    <button class="btn-icon delete" title="Unpair" on:click={() => deleteServer(node.instance_id)}>&#128465;</button>
                                </td>
                            </tr>
                        {/each}
                    </tbody>
                </table>
            {/if}
        </div>
    {/if}
</div>

<style>
    .page { padding: 2rem; max-width: 1100px; margin: 0 auto; }
    header { display: flex; justify-content: space-between; align-items: center; margin-bottom: 2rem; flex-wrap: wrap; gap: 1rem; }
    h1 { color: #fff; margin: 0; font-size: 1.8rem; }

    .tabs { display: flex; gap: 8px; }
    .tab { background: #333; border: none; color: #aaa; padding: 10px 20px; border-radius: 6px; cursor: pointer; font-weight: 600; transition: all 0.2s; }
    .tab.active { background: #4a69bd; color: white; }
    .tab:hover:not(.active) { background: #444; }

    .toolbar { display: flex; justify-content: space-between; align-items: center; margin-bottom: 1.5rem; background: #1e1e1e; padding: 1rem; border-radius: 8px; border: 1px solid #333; flex-wrap: wrap; gap: 10px; }
    .action-group { display: flex; gap: 10px; }
    .join-group { display: flex; gap: 10px; }
    .join-group input { padding: 8px 12px; border-radius: 4px; border: 1px solid #444; background: #111; color: #fff; width: 120px; text-align: center; font-family: monospace; font-size: 1.1rem; letter-spacing: 2px; }

    .qr-panel { background: #fff; padding: 2rem; border-radius: 12px; text-align: center; margin-bottom: 2rem; color: #000; max-width: 400px; margin-left: auto; margin-right: auto; border: 4px solid transparent; }
    .qr-panel.vip { border-color: #f39c12; background: #fff9e6; }
    .qr-panel img { max-width: 100%; height: auto; display: block; margin: 0 auto; border: 1px solid #eee; }
    .hint { margin-top: 1rem; font-size: 0.9rem; color: #555; }

    .list-container { background: #1e1e1e; border-radius: 8px; border: 1px solid #333; overflow: hidden; }
    table { width: 100%; border-collapse: collapse; color: #eee; }
    th { text-align: left; padding: 1rem; background: #252525; border-bottom: 1px solid #333; color: #888; font-size: 0.8rem; text-transform: uppercase; font-weight: 600; }
    td { padding: 1rem; border-bottom: 1px solid #2a2a2a; vertical-align: middle; }
    tr:last-child td { border-bottom: none; }
    tr.deleted { opacity: 0.5; background: #2a1a1a; }

    .mono { font-family: monospace; color: #aaa; font-size: 0.9rem; }
    .mono-id { font-family: monospace; color: #fff; font-weight: bold; }
    .mono-key { font-family: monospace; color: #666; font-size: 0.8em; }
    .mono-sm { font-family: monospace; color: #aaa; font-size: 0.8rem; background: #111; padding: 4px 8px; border-radius: 4px; display: inline-block; }

    .badge { padding: 4px 8px; border-radius: 4px; font-size: 0.75rem; font-weight: bold; text-transform: uppercase; }
    .badge.active { background: rgba(40, 167, 69, 0.2); color: #28a745; }
    .badge.pending { background: rgba(255, 193, 7, 0.2); color: #ffc107; }
    .badge.blocked { background: rgba(220, 53, 69, 0.2); color: #dc3545; }
    .badge.deleted { background: #333; color: #aaa; text-decoration: line-through; }

    .role-badge { padding: 4px 8px; border-radius: 4px; font-size: 0.75rem; font-weight: bold; text-transform: uppercase; }
    .role-badge.master { background: rgba(243, 156, 18, 0.2); color: #f39c12; }
    .role-badge.peer { background: rgba(74, 105, 189, 0.2); color: #4a69bd; }

    .node-select { background: #111; border: 1px solid #444; color: #ddd; padding: 6px; border-radius: 4px; max-width: 200px; }

    .dot { height: 8px; width: 8px; background-color: #dc3545; border-radius: 50%; display: inline-block; margin-right: 6px; }
    .dot.online { background-color: #28a745; box-shadow: 0 0 5px #28a745; }
    .dot.degraded { background-color: #ffc107; box-shadow: 0 0 5px #ffc107; }

    .btn { padding: 0.6rem 1.2rem; border-radius: 6px; border: 1px solid transparent; font-weight: 600; cursor: pointer; transition: all 0.2s; }
    .btn.active { transform: translateY(2px); box-shadow: inset 0 2px 4px rgba(0,0,0,0.2); }
    .btn.primary { background: #4a69bd; color: white; }
    .btn.primary:hover { background: #3a59ad; }
    .btn.secondary { background: #2a2a2a; color: #fff; border-color: #444; }
    .btn.secondary:hover { background: #3a3a3a; }
    .btn:disabled { opacity: 0.5; cursor: not-allowed; }
    .btn-text { background: none; border: none; color: #666; text-decoration: underline; margin-top: 10px; cursor: pointer; }
    .btn-icon { background: none; border: none; font-size: 1.2rem; cursor: pointer; padding: 4px; transition: transform 0.2s; color: #aaa; }
    .btn-icon:hover { transform: scale(1.2); }
    .btn-icon.approve { color: #28a745; }
    .btn-icon.block { color: #dc3545; }
    .btn-icon.delete { color: #888; }
    .btn-icon.delete:hover { color: #dc3545; }

    /* Pairing Modal */
    .pairing-modal { position: fixed; top: 0; left: 0; width: 100%; height: 100%; background: rgba(0,0,0,0.8); display: flex; justify-content: center; align-items: center; z-index: 1000; }
    .modal-content { background: #252525; padding: 2.5rem; border-radius: 12px; border: 1px solid #444; text-align: center; min-width: 320px; color: #fff; }
    .modal-content h3 { margin-top: 0; }
    .big-code { font-size: 2.5rem; font-family: monospace; letter-spacing: 4px; color: #4a69bd; margin: 1rem 0; font-weight: bold; background: #111; padding: 12px; border-radius: 8px; border: 1px dashed #444; }
    .spinner { width: 30px; height: 30px; border: 3px solid #444; border-top: 3px solid #4a69bd; border-radius: 50%; animation: spin 1s linear infinite; margin: 20px auto; }
    @keyframes spin { 0% { transform: rotate(0deg); } 100% { transform: rotate(360deg); } }
    .modal-actions { display: flex; gap: 10px; justify-content: center; margin-top: 20px; }

    .empty, .loading { padding: 3rem; text-align: center; color: #666; font-style: italic; }

    .identity-card { background: #1a1f2e; border: 1px solid rgba(74, 105, 189, 0.3); border-radius: 8px; padding: 1rem 1.5rem; margin-bottom: 1.5rem; }
    .identity-card h3 { margin: 0 0 0.8rem 0; color: #7b9ff0; font-size: 0.9rem; text-transform: uppercase; letter-spacing: 1px; }
    .identity-row { display: flex; gap: 1rem; padding: 4px 0; align-items: baseline; }
    .identity-label { color: #888; font-size: 0.8rem; min-width: 90px; flex-shrink: 0; }
    .identity-value { color: #ddd; font-size: 0.85rem; word-break: break-all; }
    .identity-value.mono { font-family: monospace; color: #aaa; font-size: 0.8rem; }
</style>
