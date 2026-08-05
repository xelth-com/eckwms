<script>
    import { authStore } from "$lib/stores/authStore";
    import { wsStore } from "$lib/stores/wsStore";
    import { toastStore } from "$lib/stores/toastStore";
    import ToastContainer from "$lib/components/ToastContainer.svelte";
    import MeshStatus from "$lib/components/MeshStatus.svelte";
    import LanguageCycleButton from "$lib/components/LanguageCycleButton.svelte";
    import ChangePasswordModal from "$lib/components/ChangePasswordModal.svelte";
    import { KEEP_ALIVE_VIEWS, KEEP_ALIVE_MAX, KEEP_ALIVE_TTL_MS, keepAliveEntry } from "$lib/keepAlive";
    import { goto } from "$app/navigation";
    import { onMount, onDestroy } from "svelte";
    import { page } from "$app/stores";
    import { base } from "$app/paths";
    import { t, tr, locale, availableLocales, localeName, setLocale, refreshAvailableLocales } from "$lib/i18n";

    // Settings language switcher (visible to all authenticated users).
    // The admin sees one extra row in the list — "Add language…" — which
    // morphs the select into a 2-letter code input (Enter starts the job,
    // Escape returns to the select).
    const ADD_LANG_OPTION = "__add__";
    let addMode = false;

    function onLangSelect(e) {
        if (e.target.value === ADD_LANG_OPTION) {
            e.target.value = $locale; // don't let the sentinel stick as selection
            addMode = true;
            return;
        }
        setLocale(e.target.value, $authStore.currentUser?.id ?? null);
    }

    function cancelAddLang() {
        if (addLangState === "running") return; // job in flight — keep showing it
        addMode = false;
        newLang = "";
        addLangState = "";
        addLangError = "";
    }

    function focusOnMount(node) {
        node.focus();
    }

    // ── Admin: add a UI language at runtime ─────────────────────────────────
    // The server machine-translates the whole English label set into the new
    // language (background job); we poll its status and refresh the picker on
    // completion.
    let newLang = "";
    let addLangState = ""; // '' | 'running' | 'done' | 'failed'
    let addLangDone = 0;
    let addLangTotal = 0;
    let addLangError = "";
    let addLangPoll = null;

    async function addLanguage() {
        const code = (newLang || "").trim().toLowerCase();
        if (!/^[a-z]{2}$/.test(code)) {
            addLangState = "failed";
            addLangError = tr("shell.add_language_invalid");
            return;
        }
        addLangState = "running";
        addLangError = "";
        addLangDone = 0;
        addLangTotal = 0;
        try {
            const res = await fetch("/api/admin/i18n/languages", {
                method: "POST",
                headers: {
                    "Content-Type": "application/json",
                    Authorization: `Bearer ${$authStore.token}`,
                },
                body: JSON.stringify({ lang: code }),
            });
            if (res.status === 409) {
                // Already running (single-flight) — just attach to its status.
                pollAddLang(code);
                return;
            }
            if (!res.ok) {
                const err = await res.json().catch(() => ({}));
                throw new Error(err.error || `HTTP ${res.status}`);
            }
            pollAddLang(code);
        } catch (e) {
            addLangState = "failed";
            addLangError = e.message;
        }
    }

    function pollAddLang(code) {
        if (addLangPoll) clearInterval(addLangPoll);
        addLangPoll = setInterval(async () => {
            try {
                const res = await fetch(`/api/admin/i18n/languages/${code}/status`, {
                    headers: { Authorization: `Bearer ${$authStore.token}` },
                });
                if (!res.ok) return;
                const data = await res.json();
                const s = data.status || {};
                addLangState = s.state || "running";
                addLangDone = s.done || 0;
                addLangTotal = s.total || 0;
                addLangError = s.error || "";
                if (s.state === "done" || s.state === "failed") {
                    clearInterval(addLangPoll);
                    addLangPoll = null;
                    if (s.state === "done") {
                        await refreshAvailableLocales($authStore.token);
                        newLang = "";
                        addMode = false; // input reverts to the (now longer) select
                        toastStore.add(tr("shell.add_language_done"), "success");
                    }
                }
            } catch {
                /* transient — keep polling */
            }
        }, 1500);
    }

    // ── POS module (paid tier) ──────────────────────────────────────────────
    // The register entry point + the kiosk boot-mode control appear only when
    // the server has POS mounted (POS_ENABLED). Kiosk config is node-local.
    let posEnabled = false;
    let kioskEnabled = false;
    let kioskMode = "wms";
    // Demo/exhibition nodes expose an external site as an extra nav entry;
    // stock nodes return no URL and the entry never renders.
    let navSiteUrl = "";
    // Shared demo account advertised to anonymous (kiosk-observer) visitors.
    let demoLoginHint = "";
    // License state ("licensed" | "grace" | "unlicensed") — drives the grace
    // warning banner below. Grace = the register still runs on borrowed time.
    let posLicense = "";
    let graceBannerDismissed = false;

    async function loadPosStatus() {
        try {
            const res = await fetch("/api/pos/status");
            if (res.ok) {
                const status = await res.json();
                posEnabled = status.enabled === true;
                navSiteUrl = typeof status.nav_site_url === "string" ? status.nav_site_url : "";
                demoLoginHint = typeof status.demo_login_hint === "string" ? status.demo_login_hint : "";
                posLicense = typeof status.license === "string" ? status.license : "";
            }
        } catch { /* older server without the endpoint — keep hidden */ }
    }

    $: showGraceBanner =
        posLicense === "grace" &&
        !graceBannerDismissed &&
        ($authStore.currentUser?.role === "admin" || $authStore.currentUser?.role === "operator");

    async function loadKioskConfig() {
        try {
            const res = await fetch("/api/admin/config/kiosk", {
                headers: { Authorization: `Bearer ${$authStore.token}` },
            });
            if (res.ok) {
                const cfg = await res.json();
                kioskEnabled = cfg.enabled === true;
                kioskMode = cfg.mode === "pos" ? "pos" : "wms";
            }
        } catch { /* control keeps its defaults */ }
    }

    async function saveKioskConfig() {
        try {
            const res = await fetch("/api/admin/config/kiosk", {
                method: "POST",
                headers: {
                    "Content-Type": "application/json",
                    Authorization: `Bearer ${$authStore.token}`,
                },
                body: JSON.stringify({ enabled: kioskEnabled, mode: kioskMode }),
            });
            if (!res.ok) {
                const err = await res.json().catch(() => ({}));
                throw new Error(err.error || `HTTP ${res.status}`);
            }
            toastStore.add(tr("shell.kiosk_config_saved"), "success");
        } catch (e) {
            toastStore.add(tr("shell.kiosk_config_failed", { error: e.message }), "error");
        }
    }

    function openPos() {
        // Fullscreen needs a user gesture. Chromium exits fullscreen on
        // navigation, so the POS app re-requests it on its first tap too.
        const go = () => { window.location.href = "/K/"; };
        try {
            const p = document.documentElement.requestFullscreen?.();
            if (p && typeof p.then === "function") p.then(go, go);
            else go();
        } catch {
            go();
        }
    }

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

    // The dashboard index hosts the full-bleed logistics map: it fills the whole
    // right pane and must NOT scroll (the map resizes itself instead). Every
    // other dashboard page keeps the padded, scrollable .content.
    // ── Keep-alive cache (see lib/keepAlive.js) ─────────────────────────────
    // Current route path with the base prefix stripped (so it matches the
    // registry's relative paths, e.g. "/dashboard/support").
    $: relPath = (() => {
        const p = $page.url.pathname;
        const r = base && p.startsWith(base) ? p.slice(base.length) : p;
        return r || "/";
    })();
    $: currentEntry = keepAliveEntry(relPath);
    // The map view is full-bleed; everything else keeps the padded .content.
    // The embedded marketing site (/dashboard/site/*) fills the pane the same way.
    $: isFullbleed = !!(currentEntry && currentEntry.fullbleed) || relPath.startsWith("/dashboard/site");

    // LRU of mounted views: [{ path, lastUsed }], most-recently-used first.
    let cached = [];
    $: if (currentEntry) touchCache(currentEntry.path);
    function touchCache(path) {
        const t = Date.now();
        cached = [{ path, lastUsed: t }, ...cached.filter((c) => c.path !== path)]
            .slice(0, KEEP_ALIVE_MAX);
    }
    function compForPath(path) {
        return KEEP_ALIVE_VIEWS.find((v) => v.path === path)?.component;
    }
    function isFullbleedPath(path) {
        return !!KEEP_ALIVE_VIEWS.find((v) => v.path === path)?.fullbleed;
    }
    // TTL: drop hidden views idle past the threshold (never the current one).
    onMount(() => {
        const iv = setInterval(() => {
            const t = Date.now();
            cached = cached.filter(
                (c) => c.path === relPath || t - c.lastUsed < KEEP_ALIVE_TTL_MS,
            );
        }, 60_000);
        return () => clearInterval(iv);
    });

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
            toastStore.add(tr("shell.toast_remote_approved"), "success");
            showXelixirApproval = false;
            xelixirRequest = null;
        } catch (e) {
            toastStore.add(tr("shell.toast_approve_failed", { error: e.message }), "error");
        } finally {
            xelixirApproveBusy = false;
        }
    }

    function denyXelixir() {
        // Local denial is purely UI — the request is dropped on this client.
        // The cloud will see no status transition and can re-request later.
        showXelixirApproval = false;
        xelixirRequest = null;
        toastStore.add(tr("shell.toast_remote_denied"), "info");
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
            // Cashiers work the register — the dashboard isn't theirs.
            if (!state.isLoading && state.isAuthenticated && state.currentUser?.role === 'cashier') {
                window.location.replace('/K/');
            }
        });

        // 2. Init WebSocket
        wsStore.connect();

        // 2b. Discover customer-added languages so the picker includes them.
        refreshAvailableLocales($authStore.token);

        // 2c. POS module availability + (admin) kiosk boot-mode config.
        loadPosStatus();
        if ($authStore.currentUser?.role === 'admin') loadKioskConfig();

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
        if (addLangPoll) clearInterval(addLangPoll);
    });

    function handleLogout() {
        authStore.logout();
        wsStore.close();
        goto(`${base}/login`);
    }

    // Change-password modal. Opens on demand, or is forced (non-dismissable)
    // when the account is flagged mustChangePassword (bulk-seeded shared pw).
    let showChangePassword = false;
    $: forcedPasswordChange =
        $authStore.currentUser?.mustChangePassword === true &&
        !$authStore.isKioskObserver;

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
            copyFeedback = withPrompt ? tr("shell.copied_with_prompt") : tr("shell.copied");
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
            toastStore.add(tr("shell.toast_critical", { title: msg.title }), "error", 10000);
            return;
        }

        // Xelixir remote support: cloud requested a session and this node's
        // auto_accept is off — surface a modal for the operator to allow/deny.
        if (msg.type === "XELIXIR_REQUESTED") {
            xelixirRequest = msg;
            showXelixirApproval = true;
            toastStore.add(tr("shell.toast_remote_requesting"), "warning", 8000);
            return;
        }

        if (msg.barcode || (msg.data && msg.data.barcode)) {
            const barcode = msg.barcode || msg.data.barcode;
            processScan(barcode);
            return;
        }

        if (msg.success && msg.data) {
            toastStore.add(tr("shell.toast_operation_success"), "success");
        } else if (msg.type === "ERROR" || msg.error) {
            toastStore.add(msg.text || msg.error || tr("shell.toast_error_occurred"), "error");
        } else if (msg.text) {
            toastStore.add(msg.text, "info");
        }
    }

    async function processScan(barcode) {
        // Play sound (optional, browser policy might block)
        // const audio = new Audio('/beep.mp3'); audio.play().catch(e=>{});

        toastStore.add(tr("shell.toast_scanning"), "info", 1000);

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
                throw new Error(err.error || tr("shell.scan_failed"));
            }

            const data = await res.json();

            // Handle ambiguous collision — multiple matches
            if (data.type === "ambiguous") {
                ambiguousCandidates = data.data?.candidates || [];
                showAmbiguousModal = true;
                toastStore.add(tr("shell.toast_multiple_matches"), "warning");
                return;
            }

            // Soft trust warning
            if (data.trust === "soft") {
                toastStore.add(tr("shell.toast_soft_trust"), "warning", 4000);
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
                    tr("shell.toast_box_scanned", { name: data.data.name || data.data.id }),
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
            toastStore.add(tr("shell.toast_error", { error: e.message }), "error");
        }
    }
</script>

<div class="dashboard-shell">
    {#if showGraceBanner}
        <!-- POS license slipped into its 30-day grace period: the register
             still runs (fiscal continuity), but won't remount after grace
             ends. Slim, dismissible, admin/operator only. -->
        <div class="grace-banner">
            <span class="grace-banner-text">⚠️ {$t('shell.pos_grace_banner')}</span>
            <button class="grace-banner-dismiss" on:click={() => (graceBannerDismissed = true)}>
                {$t('shell.pos_grace_dismiss')}
            </button>
        </div>
    {/if}
    <div class="dashboard-layout">
    <aside class="sidebar">
        <div class="brand">
            <span class="brand-text">eckWMS</span>
        </div>

        <!-- Mesh Network Status -->
        <div class="mesh-section">
            <div class="section-label">{$t('shell.connected_servers')}</div>
            <MeshStatus />
        </div>

        <!-- Kiosk observer language cycle (headline feature): big tap target near
             the top of the menu. Regular users use the settings dropdown below. -->
        {#if $authStore.isKioskObserver || $authStore.currentUser?.role === 'observer'}
            <div class="lang-cycle-section">
                <LanguageCycleButton />
            </div>
        {/if}

        <!-- Exhibition-node site links (env-gated via ECK_NAV_SITE_URL): the
             demo doubles as the public site, so the marketing pages get
             prominent buttons up top instead of a buried menu entry. -->
        {#if navSiteUrl}
            <div class="site-links">
                <a class="site-link" href="{base}/dashboard/site/features"
                   class:active={relPath.startsWith("/dashboard/site/features") && !$page.url.hash}>
                    ✨ {$t('shell.site_features')}
                </a>
                <a class="site-link" href="{base}/dashboard/site/features#developers"
                   class:active={relPath.startsWith("/dashboard/site/features") && $page.url.hash === "#developers"}>
                    🧩 {$t('shell.site_developers')}
                </a>
                <a class="site-link" href="{base}/dashboard/site"
                   class:active={relPath === "/dashboard/site"}>
                    🌐 {$t('shell.nav_site')}
                </a>
            </div>
        {/if}

        <nav>
            {#if posEnabled && ($authStore.currentUser?.role === 'admin' || $authStore.currentUser?.role === 'operator')}
                <button class="nav-pos" on:click={openPos}>
                    🧾 {$t('shell.nav_pos')}
                </button>
            {/if}
            <a
                href="{base}/dashboard"
                class:active={$page.url.pathname === `${base}/dashboard` ||
                    $page.url.pathname === "/dashboard"}
            >
                {$t('shell.nav_dashboard')}
            </a>
            <a
                href="{base}/dashboard/inventory"
                class:active={$page.url.pathname.includes("/dashboard/inventory") ||
                    $page.url.pathname.includes("/items")}
            >
                {$t('shell.nav_inventory')}
            </a>
            <a
                href="{base}/dashboard/warehouse"
                class:active={$page.url.pathname.includes("/warehouse")}
            >
                {$t('shell.nav_warehouse')}
            </a>
            <a
                href="{base}/dashboard/shipping"
                class:active={$page.url.pathname.includes("/shipping")}
            >
                {$t('shell.nav_shipping')}
            </a>
            <a
                href="{base}/dashboard/rma"
                class:active={$page.url.pathname.includes("/rma")}
            >
                {$t('shell.nav_rma')}
            </a>
            <a
                href="{base}/dashboard/repairs"
                class:active={$page.url.pathname.includes("/repairs")}
            >
                {$t('shell.nav_repairs')}
            </a>
            <a
                href="{base}/dashboard/support"
                class:active={$page.url.pathname.includes("/support")}
            >
                {$t('shell.nav_support')}
            </a>
            <a
                href="{base}/dashboard/print"
                class:active={$page.url.pathname.includes("/print")}
            >
                {$t('shell.nav_printing')}
            </a>
            <a
                href="{base}/dashboard/devices"
                class:active={$page.url.pathname.includes("/devices")}
            >
                {$t('shell.nav_devices')}
            </a>
            <a
                href="{base}/dashboard/users"
                class:active={$page.url.pathname.includes("/users")}
            >
                {$t('shell.nav_users')}
            </a>
            <a
                href="{base}/dashboard/connector"
                class:active={$page.url.pathname.includes("/connector")}
            >
                {$t('shell.nav_connector')}
            </a>
            <a
                href="{base}/dashboard/scrapers"
                class:active={$page.url.pathname.includes("/scrapers")}
            >
                {$t('shell.nav_scrapers')}
            </a>
            <a
                href="{base}/dashboard/ai"
                class:active={$page.url.pathname.includes("/dashboard/ai")}
            >
                {$t('shell.nav_ai_inbox')}
            </a>
            <a
                href="{base}/dashboard/analysis"
                class:active={$page.url.pathname.includes("/analysis")}
                style="margin-top: 1rem; border-top: 1px solid #333; padding-top: 1rem;"
            >
                {$t('shell.nav_analysis')}
            </a>


            {#if activeAlert}
                <button class="nav-alert-btn" on:click={() => showAlertModal = true}>
                    {activeAlert.title}
                </button>
            {/if}

            <!-- POS not active on this node → upsell button pinned to the
                 bottom (mirrors the golden top button that paid nodes get). -->
            {#if !posEnabled && ($authStore.currentUser?.role === 'admin' || $authStore.currentUser?.role === 'operator')}
                <a
                    href="{base}/dashboard/pos-promo"
                    class="nav-pos-promo"
                    class:active={$page.url.pathname.includes("/pos-promo")}
                >
                    💶 {$t('shell.nav_pos_promo')}
                </a>
            {/if}
        </nav>

        <div class="user-panel">
            <div class="user-info">
                <span class="username"
                    >{$authStore.currentUser?.username || $t('shell.user_fallback')}</span
                >
                <span class="role"
                    >{$authStore.currentUser?.role || $t('shell.role_fallback')}{#if $authStore.isKioskObserver} {$t('shell.read_only')}{/if}</span
                >
            </div>

            {#if $authStore.isKioskObserver && demoLoginHint}
                <!-- Exhibition affordance: tell the anonymous visitor how to get
                     the writable demo account (?real=1 skips the auto-redirect). -->
                <a class="demo-login-hint" href="{base}/login?real=1">
                    {$t('shell.demo_login')}: <strong>{demoLoginHint}</strong>
                </a>
            {/if}

            <!-- Settings language switcher (all authenticated users). For the
                 admin the list carries one extra row — "Add language…" — which
                 swaps this select for a 2-letter code input in the same spot. -->
            <label class="lang-setting">
                <span class="lang-setting-label">{$t('shell.language')}</span>
                {#if addMode}
                    <input
                        class="add-lang-input"
                        type="text"
                        maxlength="2"
                        bind:value={newLang}
                        placeholder={$t('shell.add_language_hint')}
                        disabled={addLangState === 'running'}
                        use:focusOnMount
                        on:keydown={(e) => {
                            if (e.key === 'Enter') addLanguage();
                            else if (e.key === 'Escape') cancelAddLang();
                        }}
                        on:blur={() => { if (addLangState !== 'running' && !newLang) cancelAddLang(); }}
                    />
                    {#if addLangState === 'running'}
                        <span class="add-lang-status"
                            >{$t('shell.add_language_progress', { done: addLangDone, total: addLangTotal })}</span
                        >
                    {:else if addLangState === 'failed'}
                        <span class="add-lang-status err"
                            >{$t('shell.add_language_failed', { error: addLangError })}</span
                        >
                    {:else}
                        <span class="add-lang-status">{$t('shell.add_language_hint')} ⏎</span>
                    {/if}
                {:else}
                    <select
                        class="lang-select"
                        value={$locale}
                        on:change={onLangSelect}
                        aria-label={$t('shell.change_language')}
                    >
                        {#each $availableLocales as code}
                            <option value={code}>{localeName(code)}</option>
                        {/each}
                        {#if $authStore.currentUser?.role === 'admin'}
                            <option value={ADD_LANG_OPTION}>+ {$t('shell.add_language')}…</option>
                        {/if}
                    </select>
                {/if}
            </label>

            <!-- Kiosk boot config (admin, node-local): whether this node's
                 kiosk screen auto-loads, and what it loads — the observer
                 dashboard or the POS register. -->
            {#if $authStore.currentUser?.role === 'admin' && !$authStore.isKioskObserver}
                <div class="kiosk-config">
                    <label class="kiosk-config-row">
                        <input type="checkbox" bind:checked={kioskEnabled} on:change={saveKioskConfig} />
                        <span>{$t('shell.kiosk_autologin')}</span>
                    </label>
                    {#if kioskEnabled && posEnabled}
                        <label class="lang-setting">
                            <span class="lang-setting-label">{$t('shell.kiosk_boot_mode')}</span>
                            <select class="lang-select" bind:value={kioskMode} on:change={saveKioskConfig}>
                                <option value="wms">{$t('shell.kiosk_mode_wms')}</option>
                                <option value="pos">{$t('shell.kiosk_mode_pos')}</option>
                            </select>
                        </label>
                    {/if}
                </div>
            {/if}

            {#if !$authStore.isKioskObserver}
                <button on:click={() => (showChangePassword = true)} class="change-pw-btn">
                    {$t('shell.change_password')}
                </button>
            {/if}

            <button on:click={handleLogout} class="logout-btn">{$t('shell.logout')}</button>
        </div>
    </aside>

    {#if forcedPasswordChange || showChangePassword}
        <ChangePasswordModal
            forced={forcedPasswordChange}
            on:done={() => (showChangePassword = false)}
            on:close={() => (showChangePassword = false)}
        />
    {/if}

    <main class="content" class:fullbleed={isFullbleed}>
        <!-- Keep-alive cache: registered tabs (lib/keepAlive.js) stay mounted
             across navigation, shown only when current, hidden (display:none)
             but alive otherwise — so returning is instant and keeps their state. -->
        {#each cached as c (c.path)}
            <div class="ka-host" class:fullbleed-host={isFullbleedPath(c.path)}
                 style:display={c.path === relPath ? null : "none"}>
                <svelte:component this={compForPath(c.path)} active={c.path === relPath} />
            </div>
        {/each}
        <!-- Non-cached routes render normally. -->
        {#if !currentEntry}<slot />{/if}
    </main>

    <ToastContainer />

    {#if showAlertModal && activeAlert}
        <!-- svelte-ignore a11y-click-events-have-key-events -->
        <div class="modal-overlay" on:click={() => showAlertModal = false}>
            <!-- svelte-ignore a11y-click-events-have-key-events -->
            <div class="modal-card alert-modal" on:click|stopPropagation>
                <div class="alert-header">
                    <h3>{$t('shell.alert_title')}</h3>
                    <button class="close-btn" on:click={() => showAlertModal = false}>&times;</button>
                </div>

                <div class="alert-content">
                    <h4>{activeAlert.title}</h4>
                    <p class="alert-desc">{activeAlert.message}</p>
                </div>

                <div class="alert-copy-bar">
                    <button class="copy-btn" on:click={() => copyAlert(false)}>{$t('shell.copy_message')}</button>
                    <button class="copy-btn copy-ai" on:click={() => copyAlert(true)}>{$t('shell.copy_for_ai')}</button>
                    {#if copyFeedback}<span class="copy-feedback">{copyFeedback}</span>{/if}
                </div>

                <div class="alert-footer-badge">
                    {$t('shell.alert_report_sent', { date: new Date(activeAlert.timestamp).toLocaleString() })}
                </div>

                <div class="modal-actions-bar">
                    <button class="cancel-btn" on:click={() => showAlertModal = false}>{$t('shell.close')}</button>
                    <button class="candidate-btn" on:click={() => { activeAlert = null; showAlertModal = false; }}>
                        {$t('shell.acknowledge_dismiss')}
                    </button>
                </div>
            </div>
        </div>
    {/if}

    {#if showAmbiguousModal}
        <div class="modal-overlay" on:click={dismissAmbiguous}>
            <div class="modal-card" on:click|stopPropagation>
                <h3>{$t('shell.ambiguous_title')}</h3>
                <p class="modal-hint">{$t('shell.ambiguous_hint')}</p>
                <div class="candidates-list">
                    {#each ambiguousCandidates as c}
                        <button class="candidate-btn" on:click={() => resolveCandidate(c)}>
                            <span class="candidate-type" class:type-order={c.type === 'order'} class:type-item={c.type === 'item'}>{c.type}</span>
                            <span class="candidate-title">{c.title}</span>
                            {#if c.subtitle}<span class="candidate-sub">{c.subtitle}</span>{/if}
                        </button>
                    {/each}
                </div>
                <button class="cancel-btn" on:click={dismissAmbiguous}>{$t('shell.cancel')}</button>
            </div>
        </div>
    {/if}

    {#if showXelixirApproval && xelixirRequest}
        <!-- High-priority Xelixir remote-support approval modal -->
        <div class="modal-overlay">
            <div class="modal-card xelixir-modal" on:click|stopPropagation>
                <h3 style="color: #c4b5fd;">{$t('shell.xelixir_title')}</h3>
                <p class="modal-hint">
                    {$t('shell.xelixir_hint')}
                </p>
                {#if xelixirRequest.device_id}
                    <p class="mono-sm">{$t('shell.xelixir_device', { id: xelixirRequest.device_id })}</p>
                {/if}
                {#if xelixirRequest.timestamp}
                    <p class="mono-sm">{$t('shell.xelixir_requested_at', { time: new Date(xelixirRequest.timestamp).toLocaleString() })}</p>
                {/if}
                <div class="modal-actions-bar">
                    <button class="cancel-btn" on:click={denyXelixir} disabled={xelixirApproveBusy}>{$t('shell.deny')}</button>
                    <button class="quick-login-btn" on:click={approveXelixir} disabled={xelixirApproveBusy}>
                        {xelixirApproveBusy ? $t('shell.authorizing') : $t('shell.allow')}
                    </button>
                </div>
            </div>
        </div>
    {/if}

    {#if showQuickLogin}
        <div class="modal-overlay" on:click={() => showQuickLogin = false}>
            <div class="modal-card" on:click|stopPropagation>
                <h3 style="color: #4a69bd;">{$t('shell.escalation_title')}</h3>
                <p class="modal-hint">{$t('shell.escalation_hint')}</p>
                <div class="quick-login-form">
                    <input type="text" bind:value={quickLoginUser} placeholder={$t('shell.username_or_email')} disabled={quickLoginLoading} />
                    <input type="password" bind:value={quickLoginPass} placeholder={$t('shell.password')} disabled={quickLoginLoading} />
                    {#if quickLoginError}<div class="quick-login-error">{quickLoginError}</div>{/if}
                    <button class="quick-login-btn" on:click={handleQuickLogin} disabled={quickLoginLoading}>
                        {quickLoginLoading ? $t('shell.authenticating') : $t('shell.login')}
                    </button>
                    <button class="cancel-btn" on:click={() => showQuickLogin = false} style="margin-top: 0.5rem;">{$t('shell.cancel')}</button>
                </div>
            </div>
        </div>
    {/if}
    </div>
</div>

<style>
    .dashboard-shell {
        display: flex;
        flex-direction: column;
        height: 100vh;
        overflow: hidden;
    }

    /* POS license grace-period banner — slim, dismissible, sits above the
       sidebar/content grid so it never fights the full-bleed map view. */
    .grace-banner {
        flex: 0 0 auto;
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: 1rem;
        padding: 0.55rem 1.25rem;
        background: rgba(245, 185, 66, 0.14);
        border-bottom: 1px solid rgba(245, 185, 66, 0.4);
        color: #f5b942;
        font-size: 0.85rem;
    }
    .grace-banner-text { font-weight: 600; }
    .grace-banner-dismiss {
        flex: 0 0 auto;
        background: transparent;
        color: #f5b942;
        border: 1px solid rgba(245, 185, 66, 0.5);
        border-radius: 4px;
        padding: 0.25rem 0.7rem;
        font-size: 0.75rem;
        cursor: pointer;
        transition: all 0.2s;
    }
    .grace-banner-dismiss:hover {
        background: rgba(245, 185, 66, 0.2);
    }

    .dashboard-layout {
        display: grid;
        grid-template-columns: 250px 1fr;
        flex: 1;
        min-height: 0;
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

    /* Nav entries share the bordered-chip frame of the language buttons and
       site links; active = the same dark tone with a faint blue cast. */
    nav a {
        padding: 0.7rem 1rem;
        background: #2a2a2a;
        border: 1px solid #444;
        color: #aaa;
        text-decoration: none;
        border-radius: 8px;
        transition: all 0.2s;
        font-weight: 500;
    }

    nav a:hover {
        background: #333;
        color: #fff;
    }

    nav a.active {
        background: #2c3140;
        border-color: rgba(74, 105, 189, 0.55);
        color: #e5eaf2;
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

    /* POS register entry — the paid-tier headline action, styled apart
       from the plain nav links. */
    .nav-pos {
        padding: 0.8rem 1rem;
        background: rgba(245, 185, 66, 0.08);
        color: #f5b942;
        text-align: left;
        border: 1px solid rgba(245, 185, 66, 0.35);
        border-radius: 6px;
        font-size: 1rem;
        font-weight: 600;
        cursor: pointer;
        transition: all 0.2s;
        margin-bottom: 0.5rem;
    }
    .nav-pos:hover {
        background: rgba(245, 185, 66, 0.18);
        color: #ffd27a;
    }

    /* Upsell button — pinned to the bottom of the nav (margin-top:auto pushes
       it down since nav is a flex column with flex:1). */
    .nav-pos-promo {
        margin-top: auto;
        padding: 0.8rem 1rem;
        background: rgba(245, 185, 66, 0.06);
        color: #f5b942;
        text-decoration: none;
        border: 1px dashed rgba(245, 185, 66, 0.4);
        border-radius: 6px;
        font-weight: 600;
        transition: all 0.2s;
    }
    .nav-pos-promo:hover { background: rgba(245, 185, 66, 0.15); color: #ffd27a; }
    .nav-pos-promo.active { background: rgba(245, 185, 66, 0.2); color: #ffd27a; }

    .kiosk-config {
        margin-bottom: 0.75rem;
    }
    .kiosk-config-row {
        display: flex;
        align-items: center;
        gap: 0.5rem;
        color: #a8a8c0;
        font-size: 0.85rem;
        cursor: pointer;
        margin-bottom: 0.5rem;
    }
    .kiosk-config-row input[type="checkbox"] {
        accent-color: #4a69bd;
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

    .demo-login-hint {
        display: block;
        color: #8ab4f8;
        background: rgba(138, 180, 248, 0.08);
        border: 1px solid rgba(138, 180, 248, 0.25);
        border-radius: 4px;
        padding: 0.45rem 0.6rem;
        margin-bottom: 1rem;
        font-size: 0.78rem;
        text-decoration: none;
    }

    .demo-login-hint:hover {
        background: rgba(138, 180, 248, 0.16);
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

    .change-pw-btn {
        width: 100%;
        background: #2a2a2a;
        color: #a8a8c0;
        border: 1px solid #333;
        padding: 0.5rem;
        border-radius: 4px;
        cursor: pointer;
        margin-bottom: 0.5rem;
        transition: all 0.2s;
    }

    .change-pw-btn:hover {
        background: #333;
        border-color: #4a6cf7;
    }

    /* Kiosk observer language cycle placement */
    .lang-cycle-section {
        margin-bottom: 1rem;
    }

    /* Exhibition site buttons: compact bordered chips between the language
       cycle and the nav — visible, but clearly "site", not app navigation. */
    .site-links {
        display: flex;
        flex-direction: column;
        gap: 4px;
        margin-bottom: 1rem;
    }
    .site-link {
        display: block;
        padding: 0.55rem 0.9rem;
        border: 1px solid #444;
        border-radius: 6px;
        background: #2a2a2a;
        color: #b8bcc6;
        font-size: 0.85rem;
        font-weight: 600;
        text-decoration: none;
        transition: all 0.2s;
    }
    .site-link:hover {
        background: #333;
        color: #d5d9e2;
    }
    /* Active page: dark tone with a faint blue cast, matching the lang buttons */
    .site-link.active {
        background: #2c3140;
        border-color: rgba(74, 105, 189, 0.55);
        color: #e5eaf2;
    }

    /* Settings language switcher */
    .lang-setting {
        display: flex;
        flex-direction: column;
        gap: 0.3rem;
        margin-bottom: 1rem;
    }

    .lang-setting-label {
        font-size: 0.65rem;
        font-weight: 700;
        text-transform: uppercase;
        color: #666;
        letter-spacing: 0.5px;
    }

    .lang-select {
        width: 100%;
        background: #2a2a2a;
        color: #ddd;
        border: 1px solid #333;
        padding: 0.45rem 0.5rem;
        border-radius: 4px;
        font-size: 0.85rem;
        cursor: pointer;
    }

    .lang-select:focus {
        outline: none;
        border-color: #4a69bd;
    }

    /* Admin: the "Add language…" row swaps the select for this input in place. */
    .add-lang-input {
        width: 100%;
        box-sizing: border-box;
        background: #2a2a2a;
        color: #ddd;
        border: 1px solid #4a69bd;
        border-radius: 4px;
        padding: 0.45rem 0.5rem;
        font-size: 0.85rem;
        text-transform: lowercase;
    }

    .add-lang-input:focus {
        outline: none;
        border-color: #4a69bd;
    }

    .add-lang-status {
        font-size: 0.75rem;
        color: #a8a8c0;
    }

    .add-lang-status.err {
        color: #ff6b6b;
    }

    .content {
        overflow-y: auto;
        padding: 2rem 2rem 4rem 2rem;
        background: #121212;
        /* positioning context so a page can go full-bleed (e.g. the map) */
        position: relative;
    }

    /* Full-bleed pages (the logistics map) fill the pane edge-to-edge and never
       scroll — drop the padding and clip, so an inset:0 child can't spill past
       the viewport and spawn a scrollbar. The map resizes to fit instead. */
    .content.fullbleed {
        padding: 0;
        overflow: hidden;
    }

    /* Keep-alive hosts: each holds one cached tab. Flow tabs render normally;
       a full-bleed tab (the map) fills the pane. Hidden ones are display:none. */
    .ka-host { min-height: 0; }
    .ka-host.fullbleed-host { position: absolute; inset: 0; }

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
