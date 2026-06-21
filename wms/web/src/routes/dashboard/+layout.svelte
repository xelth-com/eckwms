<script>
    import { authStore } from "$lib/stores/authStore";
    import { wsStore } from "$lib/stores/wsStore";
    import { toastStore } from "$lib/stores/toastStore";
    import ToastContainer from "$lib/components/ToastContainer.svelte";
    import MeshStatus from "$lib/components/MeshStatus.svelte";
    import { goto } from "$app/navigation";
    import { onMount, onDestroy } from "svelte";
    import { page } from "$app/stores";
    import { base } from "$app/paths";

    // Ambiguous collision modal state
    let showAmbiguousModal = false;
    let ambiguousCandidates = [];

    // System Alert State
    let activeAlert = null;
    let showAlertModal = false;

    // Quick Login Modal (privilege escalation from observer)
    let showQuickLogin = false;
    let quickLoginUser = '';
    let quickLoginPass = '';
    let quickLoginError = '';
    let quickLoginLoading = false;

    // Xelixir Remote Support approval modal (driven by WS XELIXIR_REQUESTED)
    let showXelixirApproval = false;
    let xelixirRequest = null;
    let xelixirApproveBusy = false;

    function handleForbidden(e) {
        const state = authStore.getContext ? authStore.getContext() : null;
        showQuickLogin = true;
        quickLoginError = '';
        quickLoginUser = '';
        quickLoginPass = '';
    }

    async function approveXelixir() {
        xelixirApproveBusy = true;
        try {
            const res = await fetch("/X/approve", {
                method: "POST",
                headers: {
                    "Content-Type": "application/json",
                    Authorization: `Bearer ${$authStore.token}`,
                },
                body: "{}",
            });
            if (!res.ok) {
                const err = await res.json().catch(() => ({}));
                throw new Error(err.error || `Approve failed: ${res.status}`);
            }
            toastStore.add("Remote support session approved", "success");
            showXelixirApproval = false;
            xelixirRequest = null;
        } catch (e) {
            toastStore.add("Failed to approve: " + e.message, "error");
        } finally {
            xelixirApproveBusy = false;
        }
    }

    function denyXelixir() {
        // Local denial is purely UI — the request is dropped on this client.
        // The cloud will see no status transition and can re-request later.
        showXelixirApproval = false;
        xelixirRequest = null;
        toastStore.add("Remote support request denied", "info");
    }

    async function handleQuickLogin() {
        quickLoginLoading = true;
        quickLoginError = '';
        const result = await authStore.login(quickLoginUser, quickLoginPass);
        if (result.success) {
            showQuickLogin = false;
        } else {
            quickLoginError = result.error || 'Login failed';
        }
        quickLoginLoading = false;
    }

    onMount(() => {
        // 1. Auth Guard
        const unsubscribeAuth = authStore.subscribe((state) => {
            if (!state.isLoading && !state.isAuthenticated) {
                // FIX: Robust base path handling
                const pathBase = base || '/E';
                goto(`${pathBase}/login`);
            }
        });

        // 2. Init WebSocket
        wsStore.connect();

        // 3. Listen for observer forbidden events
        window.addEventListener('auth:forbidden', handleForbidden);

        return () => {
            unsubscribeAuth();
            window.removeEventListener('auth:forbidden', handleForbidden);
        };
    });

    onDestroy(() => {
        // Don't close WS on destroy of layout if navigating within dashboard,
        // but fine for now as +layout is persistent.
    });

    function handleLogout() {
        authStore.logout();
        wsStore.close();
        goto(`${base}/login`);
    }

    function resolveCandidate(candidate) {
        showAmbiguousModal = false;
        ambiguousCandidates = [];
        const id = candidate.id;
        if (candidate.type === "order") {
            goto(`${base}/dashboard/repairs/${id}`);
        } else if (candidate.type === "item") {
            goto(`${base}/dashboard/items/${id}`);
        } else if (candidate.type === "product") {
            goto(`${base}/dashboard/items/${id}`);
        }
    }

    function dismissAmbiguous() {
        showAmbiguousModal = false;
        ambiguousCandidates = [];
    }

    // Copy alert text to clipboard
    let copyFeedback = "";
    function copyAlert(withPrompt = false) {
        if (!activeAlert) return;
        let text = `${activeAlert.title}\n\n${activeAlert.message}`;
        if (withPrompt) {
            text = `Analyze this system anomaly report from eckWMS and suggest a root cause and fix:\n\n---\n${text}\n---\n\nTimestamp: ${new Date(activeAlert.timestamp).toISOString()}\nSeverity: ${activeAlert.severity || "critical"}`;
        }
        navigator.clipboard.writeText(text).then(() => {
            copyFeedback = withPrompt ? "Copied with prompt!" : "Copied!";
            setTimeout(() => copyFeedback = "", 2000);
        });
    }

    // Reactive listener for WebSocket messages
    $: if ($wsStore.lastMessage) {
        handleWsMessage($wsStore.lastMessage);
    }

    function handleWsMessage(msg) {
        // Prevent processing if message is too old (basic check)
        if (Date.now() - (msg._receivedAt || 0) > 2000) return;

        // Handle Scan Events
        // Handle System Alerts
        if (msg.type === "SYSTEM_ALERT") {
            activeAlert = msg;
            toastStore.add(`CRITICAL: ${msg.title}`, "error", 10000);
            return;
        }

        // Xelixir remote support: cloud requested a session and this node's
        // auto_accept is off — surface a modal for the operator to allow/deny.
        if (msg.type === "XELIXIR_REQUESTED") {
            xelixirRequest = msg;
            showXelixirApproval = true;
            toastStore.add("Remote Support is requesting access", "warning", 8000);
            return;
        }

        if (msg.barcode || (msg.data && msg.data.barcode)) {
            const barcode = msg.barcode || msg.data.barcode;
            processScan(barcode);
            return;
        }

        if (msg.success && msg.data) {
            toastStore.add(`Operation Success`, "success");
        } else if (msg.type === "ERROR" || msg.error) {
            toastStore.add(msg.text || msg.error || "Error occurred", "error");
        } else if (msg.text) {
            toastStore.add(msg.text, "info");
        }
    }

    async function processScan(barcode) {
        // Play sound (optional, browser policy might block)
        // const audio = new Audio('/beep.mp3'); audio.play().catch(e=>{});

        toastStore.add("Scanning...", "info", 1000);

        try {
            const res = await fetch("/api/scan", {
                method: "POST",
                headers: {
                    "Content-Type": "application/json",
                    Authorization: `Bearer ${$authStore.token}`,
                },
                body: JSON.stringify({ barcode }),
            });

            if (!res.ok) {
                const err = await res.json();
                throw new Error(err.error || "Scan failed");
            }

            const data = await res.json();

            // Handle ambiguous collision — multiple matches
            if (data.type === "ambiguous") {
                ambiguousCandidates = data.data?.candidates || [];
                showAmbiguousModal = true;
                toastStore.add("Multiple matches — please select", "warning");
                return;
            }

            // Soft trust warning
            if (data.trust === "soft") {
                toastStore.add("Opened via external code. Please verify data.", "warning", 4000);
            }

            // Show result
            toastStore.add(data.message, "success");

            // Handle Navigation / Action based on type
            if (data.type === "order" && data.data?.id) {
                goto(`${base}/dashboard/repairs/${data.data.id}`);
            } else if (data.type === "item" && data.data?.id) {
                goto(`${base}/dashboard/items/${data.data.id}`);
            } else if (data.type === "box" && data.data?.id) {
                console.log("Box scanned:", data.data);
                toastStore.add(
                    `Box ${data.data.name || data.data.id} scanned`,
                    "success",
                );
            } else if (data.type === "place" && data.data?.id) {
                goto(`${base}/dashboard/warehouse/${data.data.id}`);
            } else if (data.type === "product" && data.data?.id) {
                goto(`${base}/dashboard/items/${data.data.id}`);
            } else if (data.type === "label") {
                console.log("Label scanned:", data.data);
            }
        } catch (e) {
            console.error("Scan error:", e);
            toastStore.add(`Error: ${e.message}`, "error");
        }
    }
</script>

<div class="dashboard-layout">
    <aside class="sidebar">
        <div class="brand">
            <span class="brand-text">eckWMS</span>
        </div>

        <!-- Mesh Network Status -->
        <div class="mesh-section">
            <div class="section-label">Connected Servers:</div>
            <MeshStatus />
        </div>

        <nav>
            <a
                href="{base}/dashboard"
                class:active={$page.url.pathname === `${base}/dashboard` ||
                    $page.url.pathname === "/dashboard"}
            >
                Dashboard
            </a>
            <a
                href="{base}/dashboard/items"
                class:active={$page.url.pathname.includes("/items")}
            >
                Inventory
            </a>
            <a
                href="{base}/dashboard/warehouse"
                class:active={$page.url.pathname.includes("/warehouse")}
            >
                Warehouse
            </a>
            <a
                href="{base}/dashboard/shipping"
                class:active={$page.url.pathname.includes("/shipping")}
            >
                Shipping
            </a>
            <a
                href="{base}/dashboard/rma"
                class:active={$page.url.pathname.includes("/rma")}
            >
                RMA Requests
            </a>
            <a
                href="{base}/dashboard/repairs"
                class:active={$page.url.pathname.includes("/repairs")}
            >
                Repairs
            </a>
            <a
                href="{base}/dashboard/support"
                class:active={$page.url.pathname.includes("/support")}
            >
                Support
            </a>
            <a
                href="{base}/dashboard/print"
                class:active={$page.url.pathname.includes("/print")}
            >
                Printing
            </a>
            <a
                href="{base}/dashboard/devices"
                class:active={$page.url.pathname.includes("/devices")}
            >
                Devices
            </a>
            <a
                href="{base}/dashboard/users"
                class:active={$page.url.pathname.includes("/users")}
            >
                Users
            </a>
            <a
                href="{base}/dashboard/scrapers"
                class:active={$page.url.pathname.includes("/scrapers")}
            >
                Scrapers
            </a>
            <a
                href="{base}/dashboard/ai"
                class:active={$page.url.pathname.includes("/dashboard/ai")}
            >
                AI Operator Inbox
            </a>
            <a
                href="{base}/dashboard/analysis"
                class:active={$page.url.pathname.includes("/analysis")}
                style="margin-top: 1rem; border-top: 1px solid #333; padding-top: 1rem;"
            >
                Analysis
            </a>

            {#if activeAlert}
                <button class="nav-alert-btn" on:click={() => showAlertModal = true}>
                    {activeAlert.title}
                </button>
            {/if}
        </nav>

        <div class="user-panel">
            <div class="user-info">
                <span class="username"
                    >{$authStore.currentUser?.username || "User"}</span
                >
                <span class="role"
                    >{$authStore.currentUser?.role || "Operator"}{#if $authStore.isKioskObserver} (read-only){/if}</span
                >
            </div>
            <button on:click={handleLogout} class="logout-btn">Logout</button>
        </div>
    </aside>

    <main class="content">
        <slot />
    </main>

    <ToastContainer />

    {#if showAlertModal && activeAlert}
        <!-- svelte-ignore a11y-click-events-have-key-events -->
        <div class="modal-overlay" on:click={() => showAlertModal = false}>
            <!-- svelte-ignore a11y-click-events-have-key-events -->
            <div class="modal-card alert-modal" on:click|stopPropagation>
                <div class="alert-header">
                    <h3>System Anomaly Detected</h3>
                    <button class="close-btn" on:click={() => showAlertModal = false}>&times;</button>
                </div>

                <div class="alert-content">
                    <h4>{activeAlert.title}</h4>
                    <p class="alert-desc">{activeAlert.message}</p>
                </div>

                <div class="alert-copy-bar">
                    <button class="copy-btn" on:click={() => copyAlert(false)}>Copy Message</button>
                    <button class="copy-btn copy-ai" on:click={() => copyAlert(true)}>Copy for AI</button>
                    {#if copyFeedback}<span class="copy-feedback">{copyFeedback}</span>{/if}
                </div>

                <div class="alert-footer-badge">
                    Encrypted report automatically sent to xelth.com support on {new Date(activeAlert.timestamp).toLocaleString()}
                </div>

                <div class="modal-actions-bar">
                    <button class="cancel-btn" on:click={() => showAlertModal = false}>Close</button>
                    <button class="candidate-btn" on:click={() => { activeAlert = null; showAlertModal = false; }}>
                        Acknowledge &amp; Dismiss
                    </button>
                </div>
            </div>
        </div>
    {/if}

    {#if showAmbiguousModal}
        <div class="modal-overlay" on:click={dismissAmbiguous}>
            <div class="modal-card" on:click|stopPropagation>
                <h3>Multiple Matches Found</h3>
                <p class="modal-hint">This barcode matched multiple records. Select the correct one:</p>
                <div class="candidates-list">
                    {#each ambiguousCandidates as c}
                        <button class="candidate-btn" on:click={() => resolveCandidate(c)}>
                            <span class="candidate-type" class:type-order={c.type === 'order'} class:type-item={c.type === 'item'}>{c.type}</span>
                            <span class="candidate-title">{c.title}</span>
                            {#if c.subtitle}<span class="candidate-sub">{c.subtitle}</span>{/if}
                        </button>
                    {/each}
                </div>
                <button class="cancel-btn" on:click={dismissAmbiguous}>Cancel</button>
            </div>
        </div>
    {/if}

    {#if showXelixirApproval && xelixirRequest}
        <!-- High-priority Xelixir remote-support approval modal -->
        <div class="modal-overlay">
            <div class="modal-card xelixir-modal" on:click|stopPropagation>
                <h3 style="color: #c4b5fd;">Remote Support Access Request</h3>
                <p class="modal-hint">
                    Remote Support (Xelixir) is requesting an interactive session on this device.
                    Approving will start the agent and give support staff temporary control.
                </p>
                {#if xelixirRequest.device_id}
                    <p class="mono-sm">Device: {xelixirRequest.device_id}</p>
                {/if}
                {#if xelixirRequest.timestamp}
                    <p class="mono-sm">Requested at: {new Date(xelixirRequest.timestamp).toLocaleString()}</p>
                {/if}
                <div class="modal-actions-bar">
                    <button class="cancel-btn" on:click={denyXelixir} disabled={xelixirApproveBusy}>Deny</button>
                    <button class="quick-login-btn" on:click={approveXelixir} disabled={xelixirApproveBusy}>
                        {xelixirApproveBusy ? 'Authorizing…' : 'Allow'}
                    </button>
                </div>
            </div>
        </div>
    {/if}

    {#if showQuickLogin}
        <div class="modal-overlay" on:click={() => showQuickLogin = false}>
            <div class="modal-card" on:click|stopPropagation>
                <h3 style="color: #4a69bd;">Privilege Escalation Required</h3>
                <p class="modal-hint">This action requires elevated permissions. Log in with your credentials to proceed.</p>
                <div class="quick-login-form">
                    <input type="text" bind:value={quickLoginUser} placeholder="Username or Email" disabled={quickLoginLoading} />
                    <input type="password" bind:value={quickLoginPass} placeholder="Password" disabled={quickLoginLoading} />
                    {#if quickLoginError}<div class="quick-login-error">{quickLoginError}</div>{/if}
                    <button class="quick-login-btn" on:click={handleQuickLogin} disabled={quickLoginLoading}>
                        {quickLoginLoading ? 'Authenticating...' : 'Login'}
                    </button>
                    <button class="cancel-btn" on:click={() => showQuickLogin = false} style="margin-top: 0.5rem;">Cancel</button>
                </div>
            </div>
        </div>
    {/if}
</div>

<style>
    .dashboard-layout {
        display: grid;
        grid-template-columns: 250px 1fr;
        height: 100vh;
        overflow: hidden;
    }

    .sidebar {
        background: #1e1e1e;
        border-right: 1px solid #333;
        display: flex;
        flex-direction: column;
        padding: 1rem;
        overflow-y: auto;
    }

    .brand {
        padding: 1rem 0 2rem 0;
        text-align: center;
        display: flex;
        flex-direction: column;
        align-items: center;
        gap: 0.5rem;
    }

    .brand-text {
        font-size: 1.5rem;
        font-weight: 800;
        color: #4a69bd;
        letter-spacing: 1px;
    }

    .mesh-section {
        padding: 0 1rem 1rem 1rem;
        border-bottom: 1px solid #2a2a2a;
        margin-bottom: 1rem;
    }

    .section-label {
        font-size: 0.65rem;
        font-weight: 700;
        text-transform: uppercase;
        color: #666;
        margin-bottom: 6px;
        letter-spacing: 0.5px;
    }

    nav {
        flex: 1;
        display: flex;
        flex-direction: column;
        gap: 0.5rem;
    }

    nav a {
        padding: 0.8rem 1rem;
        color: #aaa;
        text-decoration: none;
        border-radius: 6px;
        transition: all 0.2s;
        font-weight: 500;
    }

    nav a:hover {
        background: #2a2a2a;
        color: #fff;
    }

    nav a.active {
        background: #4a69bd;
        color: white;
    }

    .nav-alert-btn {
        padding: 0.8rem 1rem;
        background: #dc3545;
        color: white;
        text-align: left;
        border: none;
        border-radius: 6px;
        font-weight: bold;
        cursor: pointer;
        animation: blink-bg 2s infinite;
    }
    @keyframes blink-bg {
        0% { background-color: #dc3545; }
        50% { background-color: #991b1b; }
        100% { background-color: #dc3545; }
    }

    .alert-modal { border-color: #dc3545; }
    .alert-header { display: flex; justify-content: space-between; align-items: center; }
    .alert-header h3 { color: #ff6b6b; margin: 0; }
    .close-btn { background: none; border: none; color: #888; font-size: 1.5rem; cursor: pointer; }
    .alert-content h4 { color: #fff; font-size: 1.1rem; margin: 1rem 0 0.5rem; }
    .alert-desc { color: #ccc; line-height: 1.5; font-family: monospace; background: #1a1010; padding: 1rem; border-left: 3px solid #dc3545; border-radius: 4px; }
    .alert-copy-bar { display: flex; align-items: center; gap: 0.5rem; margin-top: 1rem; }
    .copy-btn { background: #2a2a2a; color: #aaa; border: 1px solid #444; padding: 0.4rem 0.8rem; border-radius: 4px; cursor: pointer; font-size: 0.8rem; transition: all 0.15s; }
    .copy-btn:hover { background: #333; color: #fff; border-color: #666; }
    .copy-btn.copy-ai { color: #a78bfa; border-color: #5b21b6; }
    .copy-btn.copy-ai:hover { background: #2d1a4e; color: #c4b5fd; }
    .copy-feedback { font-size: 0.75rem; color: #4ade80; }
    .alert-footer-badge { margin-top: 1rem; font-size: 0.8rem; color: #a3bffa; background: rgba(74, 105, 189, 0.1); padding: 0.5rem; border-radius: 4px; text-align: center; border: 1px solid rgba(74, 105, 189, 0.3); }
    .modal-actions-bar { display: flex; gap: 0.5rem; margin-top: 1rem; }

    .user-panel {
        border-top: 1px solid #333;
        padding-top: 1rem;
        margin-top: 1rem;
    }

    .user-info {
        display: flex;
        flex-direction: column;
        margin-bottom: 1rem;
    }

    .username {
        color: #fff;
        font-weight: 600;
    }

    .role {
        color: #666;
        font-size: 0.8rem;
        text-transform: uppercase;
    }

    .logout-btn {
        width: 100%;
        background: #2a2a2a;
        color: #ff6b6b;
        border: 1px solid #333;
        padding: 0.5rem;
        border-radius: 4px;
        cursor: pointer;
        transition: all 0.2s;
    }

    .logout-btn:hover {
        background: #333;
        border-color: #ff6b6b;
    }

    .content {
        overflow-y: auto;
        padding: 2rem 2rem 4rem 2rem;
        background: #121212;
        /* positioning context so a page can go full-bleed (e.g. the map) */
        position: relative;
    }

    /* Ambiguous collision modal */
    .modal-overlay { position: fixed; inset: 0; background: rgba(0,0,0,0.7); z-index: 9999; display: flex; align-items: center; justify-content: center; }
    .modal-card { background: #1e1e1e; border: 1px solid #444; border-radius: 12px; padding: 2rem; max-width: 500px; width: 90%; max-height: 80vh; overflow-y: auto; }
    .modal-card h3 { color: #fbbf24; margin: 0 0 0.5rem; font-size: 1.2rem; }
    .modal-hint { color: #888; font-size: 0.85rem; margin-bottom: 1.25rem; }
    .candidates-list { display: flex; flex-direction: column; gap: 0.5rem; margin-bottom: 1rem; }
    .candidate-btn { display: flex; align-items: center; gap: 0.75rem; background: #2a2a2a; border: 1px solid #444; border-radius: 8px; padding: 0.8rem 1rem; cursor: pointer; color: #fff; text-align: left; transition: all 0.15s; }
    .candidate-btn:hover { background: #333; border-color: #4a69bd; }
    .candidate-type { font-size: 0.7rem; text-transform: uppercase; font-weight: 700; padding: 0.2rem 0.5rem; border-radius: 4px; white-space: nowrap; }
    .candidate-type.type-order { background: #3a2a0a; color: #fb923c; }
    .candidate-type.type-item { background: #0a2a3a; color: #38bdf8; }
    .candidate-title { font-weight: 600; flex: 1; }
    .candidate-sub { font-size: 0.8rem; color: #888; }
    .cancel-btn { width: 100%; background: #333; color: #aaa; border: 1px solid #444; padding: 0.6rem; border-radius: 6px; cursor: pointer; }
    .cancel-btn:hover { background: #444; color: #fff; }

    .quick-login-form { display: flex; flex-direction: column; gap: 0.75rem; margin-top: 0.5rem; }
    .quick-login-form input { width: 100%; padding: 0.6rem; background: #1a1a1a; border: 1px solid #444; border-radius: 4px; color: #fff; font-size: 0.9rem; box-sizing: border-box; }
    .quick-login-form input:focus { outline: none; border-color: #4a69bd; }
    .quick-login-btn { width: 100%; padding: 0.6rem; background: #4a69bd; color: white; border: none; border-radius: 4px; font-size: 0.9rem; font-weight: 600; cursor: pointer; }
    .quick-login-btn:hover { background: #3d5aa8; }
    .quick-login-btn:disabled { opacity: 0.7; cursor: not-allowed; }
    .quick-login-error { color: #ff6b6b; font-size: 0.85rem; text-align: center; }
    .xelixir-modal { border-color: #a855f7; background: #1f1a2e; }
    .xelixir-modal .modal-hint { color: #cbd5e1; }
</style>
