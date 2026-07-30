<script>
    import { onMount } from 'svelte';
    import { api } from '$lib/api';
    import { authStore } from '$lib/stores/authStore';
    import { toastStore } from '$lib/stores/toastStore';
    import { base } from '$app/paths';
    import { t, tr } from '$lib/i18n';

    // Tabs
    let activeTab = 'scanners'; // 'scanners' | 'servers'

    // Scanners Data
    let devices = [];
    let meshNodes = [];
    let loading = true;
    let qrUrl = '';
    let showQr = false;
    let qrType = 'standard';

    // Xelixir C2 (Remote Support) state — managed via /X/* endpoints,
    // NOT /api/* (microservice routing prefix).
    let xelixirConfig = { auto_start: true, auto_accept: true };
    let xelixirBusy = false;
    $: isAdmin = $authStore.currentUser?.role === 'admin';

    // Dashboard SLA scale (mesh-synced via system_config:dashboard_sla)
    let slaConfig = { aging_scale_days: 7, repair_aging_scale_days: 7, trip_fade_days: 3 };
    let slaBusy = false;

    // AI address lookup (Lever 2A: Gemini + Google grounding for office-pinned
    // tickets) — backed by system_config:geo.grounding_enabled. Off by default
    // (no cloud spend / egress); see wms/src/handlers/geo.rs.
    let groundingConfig = { enabled: false };
    let groundingBusy = false;

    // Server Pairing Data
    // NOTE: The interactive pairing UI (host/join via 6-digit code) was
    // removed 2026-05-24 — the backend endpoints (/api/pairing/*) were never
    // implemented. Current peering is documented in .eck/PEERING.md: share
    // SYNC_SECRET out-of-band and set BASE_URL in each peer's .env.

    // --- SCANNERS LOGIC ---

    async function loadXelixirConfig() {
        try {
            const cfg = await api.get('/X/config');
            xelixirConfig = {
                auto_start: cfg.auto_start !== false,
                auto_accept: cfg.auto_accept !== false,
            };
        } catch (e) {
            // Non-fatal — endpoint may not be exposed via nginx yet.
            console.warn('Xelixir config load failed:', e.message);
        }
    }

    async function saveXelixirConfig(patch) {
        if (!isAdmin || xelixirBusy) return;
        xelixirBusy = true;
        try {
            const res = await api.post('/X/config', patch);
            xelixirConfig = {
                auto_start: res.auto_start !== false,
                auto_accept: res.auto_accept !== false,
            };
            toastStore.add(tr('devices.xelixir_saved'), 'success');
        } catch (e) {
            toastStore.add(tr('devices.save_failed', { error: e.message }), 'error');
        } finally {
            xelixirBusy = false;
        }
    }

    async function loadGroundingConfig() {
        try {
            const cfg = await api.get('/api/geo/grounding-config');
            groundingConfig = { enabled: cfg.grounding_enabled === true };
        } catch (e) {
            console.warn('Grounding config load failed:', e.message);
        }
    }

    async function saveGroundingConfig(enabled) {
        if (!isAdmin || groundingBusy) return;
        groundingBusy = true;
        try {
            const res = await api.post('/api/geo/grounding-config', { enabled });
            groundingConfig = { enabled: res.grounding_enabled === true };
            toastStore.add(tr('devices.grounding_saved'), 'success');
        } catch (e) {
            toastStore.add(tr('devices.save_failed', { error: e.message }), 'error');
        } finally {
            groundingBusy = false;
        }
    }

    async function requestXelixir(deviceId) {
        try {
            await api.post(`/X/devices/${deviceId}/start`, {});
            toastStore.add(tr('devices.access_requested'), 'info');
            // Allow time for mesh sync + ack from edge before reloading.
            setTimeout(loadScannersData, 5000);
        } catch (e) {
            toastStore.add(e.message, 'error');
        }
    }

    async function stopXelixir(deviceId) {
        try {
            await api.post(`/X/devices/${deviceId}/stop`, {});
            toastStore.add(tr('devices.stop_dispatched'), 'success');
            setTimeout(loadScannersData, 5000);
        } catch (e) {
            toastStore.add(e.message, 'error');
        }
    }

    async function loadScannersData() {
        loading = true;
        try {
            const [devicesData, nodesData, statusData] = await Promise.all([
                api.get('/api/admin/devices?include_deleted=true'),
                api.get('/api/mesh/nodes'),
                api.get('/api/mesh/status')
            ]);
            devices = devicesData || [];

            // Backend returns { relay, nodes }; tolerate legacy array shape.
            let nodes = Array.isArray(nodesData)
                ? nodesData
                : (nodesData && nodesData.nodes) || [];
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
            toastStore.add(tr('devices.load_devices_failed', { error: e.message }), 'error');
        } finally {
            loading = false;
        }
    }

    async function updateStatus(deviceId, status) {
        try {
            await api.put(`/api/admin/devices/${deviceId}/status`, { status });
            toastStore.add(tr('devices.device_status_updated', { status: tr('devices.status_' + status) }), 'success');
            devices = devices.map(d => d.device_id === deviceId ? { ...d, status } : d);
        } catch (e) {
            toastStore.add(e.message, 'error');
        }
    }

    async function updateHomeNode(deviceId, homeInstanceId) {
        try {
            await api.put(`/api/admin/devices/${deviceId}/home`, { homeInstanceId });
            toastStore.add(tr('devices.home_node_updated'), 'success');
        } catch (e) {
            toastStore.add(e.message, 'error');
        }
    }

    async function deleteDevice(deviceId) {
        if (!confirm(tr('devices.delete_device_confirm'))) return;
        try {
            await api.delete(`/api/admin/devices/${deviceId}`);
            toastStore.add(tr('devices.device_deleted'), 'success');
            await loadScannersData();
        } catch (e) {
            toastStore.add(e.message, 'error');
        }
    }

    async function restoreDevice(deviceId) {
        try {
            await api.post(`/api/admin/devices/${deviceId}/restore`);
            toastStore.add(tr('devices.device_restored'), 'success');
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
            // Without this check a 401/403/500 body gets turned into an <img> blob
            // and renders as a broken image ("no QR appears") instead of an error.
            if (!res.ok) {
                const msg = await res.text().catch(() => '');
                toastStore.add(tr('devices.qr_failed', { status: res.status }) + (msg ? ': ' + msg.slice(0, 120) : ''), 'error');
                showQr = false;
                return;
            }
            const blob = await res.blob();
            qrUrl = URL.createObjectURL(blob);
            showQr = true;
        } catch (e) {
            toastStore.add(tr('devices.qr_load_failed', { error: e.message }), 'error');
        }
    }

    function getNodeName(instanceId) {
        if (!instanceId) return tr('devices.node_unknown');
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
    let meshMaster = null; // instance_id of the designated mesh master, or null

    async function loadServersData() {
        loading = true;
        try {
            const [nodesBody, status] = await Promise.all([
                api.get('/api/mesh/nodes'),
                api.get('/api/mesh/status')
            ]);
            serverNodes = Array.isArray(nodesBody)
                ? nodesBody
                : (nodesBody && nodesBody.nodes) || [];
            selfInfo = status || null;
            meshMaster = (nodesBody && nodesBody.mesh_master) || status?.mesh_master || null;
        } catch (e) {
            toastStore.add(tr('devices.load_nodes_failed'), 'error');
        } finally {
            loading = false;
        }
    }

    // Designate (or transfer) the mesh master/home to a given node. Mesh-synced
    // + vclock-bumped server-side so every node converges on the same master.
    async function setMaster(instanceId) {
        if (!isAdmin) return;
        if (!confirm(tr('devices.make_master_confirm'))) return;
        try {
            await api.post('/api/admin/mesh/master', { instanceId });
            toastStore.add(tr('devices.master_updated'), 'success');
            await loadServersData();
        } catch (e) {
            toastStore.add(tr('devices.set_master_failed', { error: e.message }), 'error');
        }
    }

    async function deleteServer(id) {
        if (!confirm(tr('devices.unpair_confirm'))) return;
        try {
            await api.delete(`/api/admin/mesh/${id}`);
            toastStore.add(tr('devices.server_unpaired'), 'success');
            await loadServersData();
        } catch (e) {
            toastStore.add(e.message, 'error');
        }
    }

    // Pairing flow removed — see note at top of <script>. Manual peering
    // via SYNC_SECRET in .env, documented in .eck/PEERING.md.

    function switchTab(tab) {
        activeTab = tab;
        if (tab === 'scanners') loadScannersData();
        else loadServersData();
    }

    async function loadSlaConfig() {
        try {
            const cfg = await api.get('/api/admin/config/dashboard_sla');
            slaConfig = {
                aging_scale_days: typeof cfg.aging_scale_days === 'number' ? cfg.aging_scale_days : 7,
                repair_aging_scale_days: typeof cfg.repair_aging_scale_days === 'number' ? cfg.repair_aging_scale_days : 7,
                trip_fade_days: typeof cfg.trip_fade_days === 'number' ? cfg.trip_fade_days : 3,
            };
        } catch (e) {
            console.warn('SLA config load failed:', e.message);
        }
    }

    async function saveSlaConfig() {
        if (!isAdmin || slaBusy) return;
        slaBusy = true;
        try {
            const res = await api.post('/api/admin/config/dashboard_sla', {
                aging_scale_days: Number(slaConfig.aging_scale_days),
                repair_aging_scale_days: Number(slaConfig.repair_aging_scale_days),
                trip_fade_days: Number(slaConfig.trip_fade_days),
            });
            slaConfig = {
                aging_scale_days: res.aging_scale_days,
                repair_aging_scale_days: res.repair_aging_scale_days,
                trip_fade_days: res.trip_fade_days,
            };
            toastStore.add(tr('devices.sla_saved'), 'success');
        } catch (e) {
            toastStore.add(tr('devices.sla_save_failed', { error: e.message }), 'error');
        } finally {
            slaBusy = false;
        }
    }

    onMount(() => {
        loadScannersData();
        loadXelixirConfig();
        loadSlaConfig();
        loadGroundingConfig();
    });
</script>

<div class="page">
    <header>
        <h1>{$t('devices.page_title')}</h1>
        <div class="tabs">
            <button class="tab" class:active={activeTab === 'scanners'} on:click={() => switchTab('scanners')}>
                {$t('devices.tab_scanners')}
            </button>
            <button class="tab" class:active={activeTab === 'servers'} on:click={() => switchTab('servers')}>
                {$t('devices.tab_servers')}
            </button>
        </div>
    </header>

    {#if activeTab === 'scanners'}
        <!-- SCANNERS VIEW -->

        {#if isAdmin}
            <!-- Xelixir Remote Support settings (manages local system_config:xelixir) -->
            <div class="xelixir-settings">
                <div class="xelixir-header">
                    <h3>{$t('devices.xelixir_assist_title')}</h3>
                    <span class="xelixir-hint">{$t('devices.xelixir_hint')}</span>
                </div>
                <label class="xelixir-toggle">
                    <input
                        type="checkbox"
                        checked={xelixirConfig.auto_start}
                        disabled={xelixirBusy}
                        on:change={(e) => saveXelixirConfig({ auto_start: e.target.checked })}
                    />
                    <span>{$t('devices.auto_start_label')}</span>
                    <span class="xelixir-toggle-hint">{$t('devices.auto_start_hint')}</span>
                </label>
                <label class="xelixir-toggle">
                    <input
                        type="checkbox"
                        checked={xelixirConfig.auto_accept}
                        disabled={xelixirBusy}
                        on:change={(e) => saveXelixirConfig({ auto_accept: e.target.checked })}
                    />
                    <span>{$t('devices.auto_accept_label')}</span>
                    <span class="xelixir-toggle-hint">{$t('devices.auto_accept_hint')}</span>
                </label>
            </div>

            <!-- AI address lookup (Lever 2A): Gemini + Google grounding for support
                 tickets that couldn't be geocoded from zip/city and fell back to the
                 office pin. Backed by system_config:geo.grounding_enabled; off by
                 default (no cloud spend / egress). -->
            <div class="xelixir-settings">
                <div class="xelixir-header">
                    <h3>{$t('devices.grounding_title')}</h3>
                    <span class="xelixir-hint">{$t('devices.grounding_hint')}</span>
                </div>
                <label class="xelixir-toggle">
                    <input
                        type="checkbox"
                        checked={groundingConfig.enabled}
                        disabled={groundingBusy}
                        on:change={(e) => saveGroundingConfig(e.target.checked)}
                    />
                    <span>{$t('devices.grounding_label')}</span>
                    <span class="xelixir-toggle-hint">{$t('devices.grounding_off_hint')}</span>
                </label>
            </div>

            <!-- Dashboard SLA aging scale (mesh-synced) -->
            <div class="xelixir-settings">
                <div class="xelixir-header">
                    <h3>{$t('devices.sla_title')}</h3>
                    <span class="xelixir-hint">{$t('devices.sla_hint')}</span>
                </div>
                <label class="xelixir-toggle">
                    <span style="min-width:180px">{$t('devices.sla_tickets_label')}</span>
                    <input
                        type="number"
                        min="0.5"
                        max="60"
                        step="0.5"
                        bind:value={slaConfig.aging_scale_days}
                        disabled={slaBusy}
                        style="width:80px;padding:2px 6px;background:#222;color:#ddd;border:1px solid #444;border-radius:3px"
                    />
                    <span class="xelixir-toggle-hint">{$t('devices.sla_tickets_hint')}</span>
                </label>
                <label class="xelixir-toggle">
                    <span style="min-width:180px">{$t('devices.sla_repairs_label')}</span>
                    <input
                        type="number"
                        min="0.5"
                        max="60"
                        step="0.5"
                        bind:value={slaConfig.repair_aging_scale_days}
                        disabled={slaBusy}
                        style="width:80px;padding:2px 6px;background:#222;color:#ddd;border:1px solid #444;border-radius:3px"
                    />
                    <span class="xelixir-toggle-hint">{$t('devices.sla_repairs_hint')}</span>
                </label>
                <label class="xelixir-toggle">
                    <span style="min-width:180px">{$t('devices.sla_trip_label')}</span>
                    <input
                        type="number"
                        min="0.5"
                        max="60"
                        step="0.5"
                        bind:value={slaConfig.trip_fade_days}
                        disabled={slaBusy}
                        style="width:80px;padding:2px 6px;background:#222;color:#ddd;border:1px solid #444;border-radius:3px"
                    />
                    <span class="xelixir-toggle-hint">{$t('devices.sla_trip_hint')}</span>
                </label>
                <div style="margin-top:0.5rem">
                    <button class="btn primary" on:click={saveSlaConfig} disabled={slaBusy}>
                        {slaBusy ? $t('devices.saving') : $t('devices.save')}
                    </button>
                </div>
            </div>
        {/if}

        <div class="toolbar">
            <div class="action-group">
                <button class="btn secondary" class:active={showQr && qrType === 'standard'} on:click={() => loadQr('standard')}>
                    {$t('devices.standard_qr')}
                </button>
                <button class="btn primary" class:active={showQr && qrType === 'vip'} on:click={() => loadQr('vip')}>
                    {$t('devices.auto_approve_qr')}
                </button>
            </div>
            <button class="btn secondary" on:click={loadScannersData}>{$t('devices.refresh')}</button>
        </div>

        {#if showQr && qrUrl}
            <div class="qr-panel" class:vip={qrType === 'vip'}>
                <h3>{qrType === 'vip' ? $t('devices.auto_approve_pairing') : $t('devices.standard_pairing')}</h3>
                <img src={qrUrl} alt={$t('devices.pairing_qr_alt')} />
                <p class="hint">
                    {#if qrType === 'vip'}
                        <strong>{$t('devices.vip_warning_label')}</strong> {$t('devices.vip_warning_text')} <u>{$t('devices.vip_warning_emph')}</u>.
                    {:else}
                        {$t('devices.std_qr_pre')} <strong>{$t('devices.std_qr_pending')}</strong> {$t('devices.std_qr_post')}
                    {/if}
                </p>
                <button class="btn-text" on:click={() => showQr = false}>{$t('devices.close')}</button>
            </div>
        {/if}

        <div class="list-container">
            {#if loading}
                <div class="loading">{$t('devices.loading_devices')}</div>
            {:else if devices.length === 0}
                <div class="empty">{$t('devices.no_devices')}</div>
            {:else}
                <table>
                    <thead>
                        <tr>
                            <th>{$t('devices.th_status')}</th>
                            <th>{$t('devices.th_device_name')}</th>
                            <th>{$t('devices.th_id_key')}</th>
                            <th>{$t('devices.th_master_node')}</th>
                            <th>{$t('devices.th_xelixir')}</th>
                            <th>{$t('devices.th_last_seen')}</th>
                            <th>{$t('devices.th_actions')}</th>
                        </tr>
                    </thead>
                    <tbody>
                        {#each devices as device}
                            <tr class:deleted={device.deletedAt}>
                                <td>
                                    {#if device.deletedAt}
                                        <span class="badge deleted">{$t('devices.badge_deleted')}</span>
                                    {:else}
                                        <span class="badge {device.status}">{$t('devices.status_' + device.status)}</span>
                                    {/if}
                                </td>
                                <td>{device.device_name || $t('devices.device_unknown')}</td>
                                <td>
                                    <div class="mono-id" title={device.device_id}>{(device.device_id || '').substring(0, 8)}...</div>
                                    <div class="mono-key">{device.public_key ? device.public_key.substring(0, 8) + '...' : '-'}</div>
                                </td>
                                <td>
                                    <select
                                        value={device.home_instance_id}
                                        on:change={(e) => updateHomeNode(device.device_id, e.target.value)}
                                        disabled={!!device.deleted_at}
                                        class="node-select"
                                    >
                                        <option value={device.home_instance_id}>{getNodeName(device.home_instance_id)} {$t('devices.current_suffix')}</option>
                                        {#each meshNodes as node}
                                            {#if node.instance_id !== device.home_instance_id}
                                                <option value={node.instance_id}>{getNodeName(node.instance_id)}</option>
                                            {/if}
                                        {/each}
                                    </select>
                                </td>
                                <td class="xelixir-cell">
                                    {#if device.xelixir_status === 'running'}
                                        <span class="badge active">{$t('devices.xelixir_running')}</span>
                                        {#if device.xelixir_session_url}
                                            <a class="btn-text-link" href={device.xelixir_session_url} target="_blank" rel="noopener">{$t('devices.open_session')}</a>
                                        {/if}
                                        {#if isAdmin}
                                            <button class="btn-text-link danger" on:click={() => stopXelixir(device.device_id)}>{$t('devices.stop')}</button>
                                        {/if}
                                    {:else if device.xelixir_status === 'pending_approval'}
                                        <span class="badge pending">{$t('devices.xelixir_awaiting')}</span>
                                    {:else if device.xelixir_status === 'starting'}
                                        <span class="badge pending">{$t('devices.xelixir_starting')}</span>
                                    {:else}
                                        {#if isAdmin && !device.deleted_at}
                                            <button class="btn-text-link" on:click={() => requestXelixir(device.device_id)}>{$t('devices.request_access')}</button>
                                        {:else}
                                            <span class="mono-key">—</span>
                                        {/if}
                                    {/if}
                                </td>
                                <td>{device.last_seen_at ? new Date(device.last_seen_at).toLocaleString() : '—'}</td>
                                <td class="actions">
                                    {#if device.deleted_at}
                                        <button class="btn-icon" title={$t('devices.restore_title')} on:click={() => restoreDevice(device.device_id)}>&#9851;</button>
                                    {:else}
                                        {#if device.status === 'pending' || device.status === 'blocked'}
                                            <button class="btn-icon approve" title={$t('devices.approve_title')} on:click={() => updateStatus(device.device_id, 'active')}>&#10003;</button>
                                        {/if}
                                        {#if device.status === 'active' || device.status === 'pending'}
                                            <button class="btn-icon block" title={$t('devices.block_title')} on:click={() => updateStatus(device.device_id, 'blocked')}>&#10007;</button>
                                        {/if}
                                        <button class="btn-icon delete" title={$t('devices.delete_title')} on:click={() => deleteDevice(device.device_id)}>&#128465;</button>
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
        <div class="peering-note">
            <strong>{$t('devices.manual_peering')}</strong> {$t('devices.peering_set')} <code>SYNC_SECRET</code> {$t('devices.peering_and')} <code>BASE_URL</code> {$t('devices.peering_in_env')} <code>.env</code> ({$t('devices.peering_see_word')} <code>.eck/PEERING.md</code>). {$t('devices.peering_not_impl')}
        </div>

        {#if selfInfo}
            <div class="identity-card">
                <h3>{$t('devices.this_server')}</h3>
                <div class="identity-row">
                    <span class="identity-label">{$t('devices.label_name')}</span>
                    <span class="identity-value">{selfInfo.instance_name || '—'}</span>
                </div>
                <div class="identity-row">
                    <span class="identity-label">{$t('devices.label_instance_id')}</span>
                    <span class="identity-value mono">{selfInfo.instance_id}</span>
                </div>
                <div class="identity-row">
                    <span class="identity-label">{$t('devices.label_mesh_id')}</span>
                    <span class="identity-value mono">{selfInfo.mesh_id}</span>
                </div>
                <div class="identity-row">
                    <span class="identity-label">{$t('devices.label_base_url')}</span>
                    <span class="identity-value mono">{selfInfo.base_url || $t('devices.not_set')}</span>
                </div>
                <div class="identity-row">
                    <span class="identity-label">{$t('devices.label_role')}</span>
                    <span class="identity-value">
                        <span class="role-badge {selfInfo.role}">{$t('devices.role_' + (selfInfo.role || 'peer'))}</span>
                        {#if isAdmin && selfInfo.role !== 'master'}
                            <button class="btn-text-link" on:click={() => setMaster(selfInfo.instance_id)} style="margin-left:8px">{$t('devices.make_this_master')}</button>
                        {/if}
                        {#if !meshMaster}
                            <span class="xelixir-toggle-hint" style="margin-left:8px">{$t('devices.no_master')}</span>
                        {/if}
                    </span>
                </div>
            </div>
        {/if}

        <div class="list-container">
            {#if loading}
                <div class="loading">{$t('devices.loading_nodes')}</div>
            {:else if serverNodes.length === 0}
                <div class="empty">{$t('devices.no_servers')}</div>
            {:else}
                <table>
                    <thead>
                        <tr>
                            <th>{$t('devices.th_status')}</th>
                            <th>{$t('devices.th_name')}</th>
                            <th>{$t('devices.th_role')}</th>
                            <th>{$t('devices.th_address')}</th>
                            <th>{$t('devices.th_actions')}</th>
                        </tr>
                    </thead>
                    <tbody>
                        {#each serverNodes as node}
                            <tr>
                                <td>
                                    <span class="dot" class:online={node.status === 'online'} class:degraded={node.status === 'degraded'}></span>
                                    {node.status === 'online' ? $t('devices.status_online') : node.status === 'degraded' ? $t('devices.status_unstable') : $t('devices.status_offline')}
                                </td>
                                <td>{node.name}</td>
                                <td><span class="role-badge {node.role}">{$t('devices.role_' + node.role)}</span></td>
                                <td class="mono">{node.base_url || $t('devices.relay_only')}</td>
                                <td>
                                    {#if isAdmin && node.role !== 'master'}
                                        <button class="btn-text-link" title={$t('devices.designate_master_title')} on:click={() => setMaster(node.instance_id)}>{$t('devices.make_master')}</button>
                                    {/if}
                                    <button class="btn-icon delete" title={$t('devices.unpair_title')} on:click={() => deleteServer(node.instance_id)}>&#128465;</button>
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

    .xelixir-settings { background: #1a1f2e; border: 1px solid rgba(168, 85, 247, 0.35); border-radius: 8px; padding: 1rem 1.5rem; margin-bottom: 1.5rem; }
    .xelixir-header { display: flex; align-items: baseline; gap: 1rem; margin-bottom: 0.75rem; }
    .xelixir-header h3 { margin: 0; color: #c4b5fd; font-size: 0.9rem; text-transform: uppercase; letter-spacing: 1px; }
    .xelixir-hint { color: #777; font-size: 0.8rem; }
    .xelixir-toggle { display: flex; align-items: center; gap: 0.6rem; padding: 0.4rem 0; color: #ddd; font-size: 0.9rem; cursor: pointer; }
    .xelixir-toggle input { transform: scale(1.1); cursor: pointer; }
    .xelixir-toggle-hint { color: #666; font-size: 0.75rem; margin-left: auto; }
    .xelixir-cell { display: flex; flex-direction: column; gap: 4px; align-items: flex-start; }
    .btn-text-link { background: none; border: none; color: #4a69bd; text-decoration: underline; cursor: pointer; padding: 0; font-size: 0.85rem; }
    .btn-text-link:hover { color: #7b9ff0; }
    .btn-text-link.danger { color: #ff6b6b; }
    .btn-text-link.danger:hover { color: #ff8e8e; }
    a.btn-text-link { display: inline-block; }

    .identity-card { background: #1a1f2e; border: 1px solid rgba(74, 105, 189, 0.3); border-radius: 8px; padding: 1rem 1.5rem; margin-bottom: 1.5rem; }
    .identity-card h3 { margin: 0 0 0.8rem 0; color: #7b9ff0; font-size: 0.9rem; text-transform: uppercase; letter-spacing: 1px; }
    .identity-row { display: flex; gap: 1rem; padding: 4px 0; align-items: baseline; }
    .identity-label { color: #888; font-size: 0.8rem; min-width: 90px; flex-shrink: 0; }
    .identity-value { color: #ddd; font-size: 0.85rem; word-break: break-all; }
    .identity-value.mono { font-family: monospace; color: #aaa; font-size: 0.8rem; }
</style>
