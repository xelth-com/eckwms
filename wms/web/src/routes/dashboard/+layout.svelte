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

        return () => {
            unsubscribeAuth();
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

    // Reactive listener for WebSocket messages
    $: if ($wsStore.lastMessage) {
        handleWsMessage($wsStore.lastMessage);
    }

    function handleWsMessage(msg) {
        // Prevent processing if message is too old (basic check)
        if (Date.now() - (msg._receivedAt || 0) > 2000) return;

        // Handle Scan Events
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
                href="{base}/dashboard/analysis"
                class:active={$page.url.pathname.includes("/analysis")}
                style="margin-top: 1rem; border-top: 1px solid #333; padding-top: 1rem;"
            >
                Analysis
            </a>
        </nav>

        <div class="user-panel">
            <div class="user-info">
                <span class="username"
                    >{$authStore.currentUser?.username || "User"}</span
                >
                <span class="role"
                    >{$authStore.currentUser?.role || "Operator"}</span
                >
            </div>
            <button on:click={handleLogout} class="logout-btn">Logout</button>
        </div>
    </aside>

    <main class="content">
        <slot />
    </main>

    <ToastContainer />

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
</style>
