<script>
    import { onMount } from 'svelte';
    import { authStore } from '$lib/stores/authStore';
    import { goto } from '$app/navigation';
    import { base } from '$app/paths';
    import { t, tr } from '$lib/i18n';

    let email = '';
    let password = '';
    let error = '';
    let isLoading = false;

    let needsSetup = false;
    let setupEmail = '';
    let setupPassword = '';
    let pollInterval;
    let enableKiosk = false;
    let kioskMode = 'wms';
    let posEnabled = false;

    async function checkPosStatus() {
        try {
            const res = await fetch('/api/pos/status');
            if (res.ok) posEnabled = (await res.json()).enabled === true;
        } catch (e) {
            // older server without the endpoint — keep the mode select hidden
        }
    }

    async function checkSetupStatus() {
        try {
            const res = await fetch('/api/auth/setup-status');
            if (res.ok) {
                const data = await res.json();
                if (data.needsSetup) {
                    needsSetup = true;
                    setupEmail = data.email;
                    if (setupPassword !== data.password) {
                        setupPassword = data.password;
                        if (email === '' || email === data.email) {
                            email = data.email;
                            password = data.password;
                        }
                    }
                } else {
                    needsSetup = false;
                    if (pollInterval) {
                        clearInterval(pollInterval);
                        pollInterval = null;
                    }
                }
            }
        } catch (e) {
            // ignore — server may be temporarily down during restart
        }
    }

    onMount(() => {
        checkSetupStatus();
        checkPosStatus();
        pollInterval = setInterval(checkSetupStatus, 3000);
        return () => {
            if (pollInterval) clearInterval(pollInterval);
        };
    });

    // Kiosk auto-login: if authStore picked up a kiosk-token observer JWT
    // (config `system_config:kiosk.enabled=true` AND request came from
    // localhost), skip the password form and go straight to the dashboard.
    // Guarded on `isKioskObserver` so a real user who deliberately navigates
    // to /login to switch accounts isn't kicked back out.
    $: if ($authStore.isKioskObserver && !$authStore.isLoading) {
        goto(`${base || '/E'}/dashboard`);
    }

    async function handleLogin() {
        if (!email || !password) {
            error = tr('login.err_fill_fields');
            return;
        }

        isLoading = true;
        error = '';

        const result = await authStore.login(email, password);

        if (result.success) {
            if (enableKiosk) {
                const token = localStorage.getItem('auth_token');
                await fetch('/api/admin/config/kiosk', {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json', 'Authorization': `Bearer ${token}` },
                    body: JSON.stringify({ enabled: true, mode: kioskMode })
                }).catch(() => {});
            }
            // Cashiers work the register, not the dashboard.
            if ($authStore.currentUser?.role === 'cashier') {
                window.location.href = '/K/';
                return;
            }
            const pathBase = base || '/E';
            goto(`${pathBase}/dashboard`);
        } else {
            error = result.error || tr('login.err_login_failed');
        }
        isLoading = false;
    }
</script>

<!-- Landing-style login: hero + feature cards + the sign-in card, sized to
     fit one viewport (the kiosk screen must never scroll). No external links
     here — the kiosk browser cannot leave the WMS and gets stranded. -->
<div class="login-page">
    <nav class="navbar">
        <div class="logo">
            <span class="e-label">/E/</span>
            eckWMS <span class="badge">RUST</span>
        </div>
    </nav>

    <main class="content">
        <div class="hero-row">
            <div class="hero-text">
                <h1>{$t('shell.landing_headline')}</h1>
                <p class="description">
                    {@html $t('shell.landing_description')}
                </p>
            </div>

            <div class="login-card">
                {#if needsSetup}
                    <div class="card-title setup-mode">{$t('login.first_run')}</div>
                    <div class="setup-banner">
                        <div class="setup-title">{$t('login.setup_title')}</div>
                        <div class="setup-creds">
                            <div class="cred-row">
                                <span class="cred-label">{$t('login.email')}</span>
                                <span class="cred-value">{setupEmail}</span>
                            </div>
                            <div class="cred-row">
                                <span class="cred-label">{$t('login.password')}</span>
                                <span class="cred-value mono">{setupPassword}</span>
                            </div>
                        </div>
                        <p class="setup-hint">{$t('login.setup_hint')}</p>
                        <label class="kiosk-toggle">
                            <input type="checkbox" bind:checked={enableKiosk} />
                            <span>{$t('login.enable_kiosk')}</span>
                        </label>
                        {#if enableKiosk && posEnabled}
                            <label class="kiosk-toggle kiosk-mode-row">
                                <span>{$t('shell.kiosk_boot_mode')}</span>
                                <select bind:value={kioskMode}>
                                    <option value="wms">{$t('shell.kiosk_mode_wms')}</option>
                                    <option value="pos">{$t('shell.kiosk_mode_pos')}</option>
                                </select>
                            </label>
                        {/if}
                    </div>
                {:else}
                    <div class="card-title">{$t('login.rust_edition')}</div>
                {/if}

                <form on:submit|preventDefault={handleLogin}>
                    <div class="form-group">
                        <label for="email">{$t('login.email')}</label>
                        <input
                            type="text"
                            id="email"
                            bind:value={email}
                            placeholder="operator@eckwms.local"
                            disabled={isLoading}
                        />
                    </div>

                    <div class="form-group">
                        <label for="password">{$t('login.password')}</label>
                        <input
                            type="password"
                            id="password"
                            bind:value={password}
                            placeholder="••••••••"
                            disabled={isLoading}
                        />
                    </div>

                    {#if error}
                        <div class="error-msg">{error}</div>
                    {/if}

                    <button type="submit" disabled={isLoading}>
                        {isLoading ? $t('login.authenticating') : $t('login.login')}
                    </button>
                </form>
            </div>
        </div>

        <div class="features-grid">
            <div class="feature-card">
                <h3>🚀 {$t('shell.landing_feat_perf_title')}</h3>
                <p>{$t('shell.landing_feat_perf_desc')}</p>
            </div>
            <div class="feature-card">
                <h3>📱 {$t('shell.landing_feat_codes_title')}</h3>
                <p>{$t('shell.landing_feat_codes_desc')}</p>
            </div>
            <div class="feature-card">
                <h3>🔄 {$t('shell.landing_feat_sync_title')}</h3>
                <p>{$t('shell.landing_feat_sync_desc')}</p>
            </div>
            <div class="feature-card">
                <h3>🔒 {$t('shell.landing_feat_zk_title')}</h3>
                <p>{$t('shell.landing_feat_zk_desc')}</p>
            </div>
        </div>
    </main>
</div>

<style>
    .login-page {
        height: 100vh;
        display: flex;
        flex-direction: column;
        background-color: #121212;
        color: #e0e0e0;
        /* Fits one screen by design; if the window is truly tiny, scroll
           rather than clip the form. */
        overflow-y: auto;
    }

    .navbar {
        display: flex;
        align-items: center;
        padding: 0.8rem 2rem;
        background: rgba(30, 30, 30, 0.5);
        backdrop-filter: blur(10px);
        border-bottom: 1px solid #333;
        flex-shrink: 0;
    }

    .logo {
        font-size: 1.3rem;
        font-weight: 800;
        color: #fff;
        letter-spacing: -0.5px;
    }

    .e-label {
        font-size: 1rem;
        font-weight: 800;
        font-family: monospace;
        color: #e03c31;
        text-shadow: 0 0 10px rgba(224, 60, 49, 0.7);
        margin-right: 4px;
        vertical-align: middle;
    }

    .badge {
        background: linear-gradient(135deg, #e03c31, #ff6b35);
        font-size: 0.65rem;
        padding: 2px 8px;
        border-radius: 4px;
        vertical-align: middle;
        margin-left: 5px;
        font-weight: 700;
        letter-spacing: 1px;
        box-shadow: 0 0 12px rgba(224, 60, 49, 0.4);
        color: #fff;
    }

    .content {
        flex: 1;
        display: flex;
        flex-direction: column;
        align-items: center;
        justify-content: space-evenly;
        padding: 1.5rem 2rem;
        gap: 1.5rem;
        min-height: 0;
        background-image: radial-gradient(#2a2a2a 1px, transparent 1px);
        background-size: 30px 30px;
        max-width: 1200px;
        width: 100%;
        margin: 0 auto;
        box-sizing: border-box;
    }

    .hero-row {
        display: flex;
        align-items: center;
        justify-content: center;
        gap: 3.5rem;
        width: 100%;
    }

    .hero-text {
        max-width: 520px;
        text-align: left;
    }

    h1 {
        font-size: 2.8rem;
        line-height: 1.1;
        margin: 0 0 1rem 0;
        background: linear-gradient(to right, #fff, #aaa);
        -webkit-background-clip: text;
        -webkit-text-fill-color: transparent;
    }

    .description {
        font-size: 1.05rem;
        color: #888;
        line-height: 1.55;
        margin: 0;
    }

    .login-card {
        background: #1e1e1e;
        padding: 1.8rem;
        border-radius: 12px;
        width: 360px;
        flex-shrink: 0;
        box-shadow: 0 10px 25px rgba(0,0,0,0.5);
        border: 1px solid #333;
        box-sizing: border-box;
    }

    .card-title {
        font-size: 0.75rem;
        color: #666;
        text-transform: uppercase;
        letter-spacing: 2px;
        text-align: center;
        margin-bottom: 1.2rem;
    }

    .setup-mode {
        color: #e8a838;
    }

    .setup-banner {
        background: rgba(232, 168, 56, 0.08);
        border: 1px solid rgba(232, 168, 56, 0.3);
        border-radius: 6px;
        padding: 0.8rem 1rem;
        margin-bottom: 1.2rem;
        font-size: 0.85rem;
        color: #ccc;
    }

    .setup-title {
        font-weight: 700;
        color: #e8a838;
        text-transform: uppercase;
        letter-spacing: 1px;
        font-size: 0.72rem;
        margin-bottom: 0.5rem;
    }

    .setup-creds {
        background: rgba(0,0,0,0.3);
        border-radius: 4px;
        padding: 0.5rem 0.7rem;
        margin-bottom: 0.6rem;
    }

    .cred-row {
        display: flex;
        gap: 0.75rem;
        align-items: baseline;
        padding: 0.15rem 0;
    }

    .cred-label {
        color: #888;
        min-width: 60px;
        font-size: 0.8rem;
    }

    .cred-value {
        color: #fff;
        font-size: 0.88rem;
    }

    .cred-value.mono {
        font-family: monospace;
        font-size: 0.95rem;
        color: #e8a838;
        letter-spacing: 1px;
    }

    .setup-hint {
        color: #666;
        font-size: 0.75rem;
        margin: 0;
    }

    .form-group {
        margin-bottom: 1rem;
    }

    label {
        display: block;
        margin-bottom: 0.4rem;
        color: #aaa;
        font-size: 0.85rem;
    }

    input {
        width: 100%;
        padding: 0.65rem 0.75rem;
        background: #141414;
        border: 1px solid #444;
        border-radius: 4px;
        color: #fff;
        font-size: 1rem;
        transition: border-color 0.2s;
        box-sizing: border-box;
    }

    input:focus {
        outline: none;
        border-color: #4a69bd;
    }

    button {
        width: 100%;
        padding: 0.7rem;
        background: #4a69bd;
        color: white;
        border: none;
        border-radius: 4px;
        font-size: 1rem;
        font-weight: 600;
        cursor: pointer;
        transition: background 0.2s;
        box-shadow: 0 4px 15px rgba(74, 105, 189, 0.25);
    }

    button:hover:not(:disabled) {
        background: #3d5aa8;
    }

    button:disabled {
        opacity: 0.7;
        cursor: not-allowed;
    }

    .error-msg {
        color: #ff6b6b;
        background: rgba(255, 107, 107, 0.1);
        padding: 0.6rem;
        border-radius: 4px;
        margin-bottom: 1rem;
        font-size: 0.88rem;
        text-align: center;
    }

    .kiosk-toggle {
        display: flex;
        align-items: center;
        gap: 0.5rem;
        margin-top: 0.6rem;
        cursor: pointer;
        font-size: 0.8rem;
        color: #aaa;
    }

    .kiosk-toggle input[type="checkbox"] {
        width: auto;
        accent-color: #4a69bd;
    }

    .kiosk-mode-row select {
        background: #1a1a1a;
        color: #eee;
        border: 1px solid #333;
        border-radius: 4px;
        padding: 2px 6px;
        font-size: 0.8rem;
    }

    .features-grid {
        display: grid;
        grid-template-columns: repeat(4, 1fr);
        gap: 1.2rem;
        width: 100%;
        text-align: left;
        flex-shrink: 0;
    }

    .feature-card {
        background: #1e1e1e;
        border: 1px solid #333;
        padding: 1.1rem 1.2rem;
        border-radius: 10px;
        transition: border-color 0.2s;
    }

    .feature-card:hover {
        border-color: #4a69bd;
    }

    .feature-card h3 {
        margin: 0 0 0.4rem 0;
        color: #e0e0e0;
        font-size: 0.95rem;
    }

    .feature-card p {
        color: #888;
        font-size: 0.8rem;
        line-height: 1.45;
        margin: 0;
    }

    /* Narrow windows: stack the hero, features go 2×2, page may scroll. */
    @media (max-width: 980px) {
        .hero-row {
            flex-direction: column;
            gap: 1.5rem;
        }
        .hero-text {
            text-align: center;
        }
        h1 { font-size: 2.2rem; }
        .features-grid {
            grid-template-columns: repeat(2, 1fr);
        }
    }

    /* Short windows (e.g. small laptop with browser chrome): tighten up. */
    @media (max-height: 760px) {
        h1 { font-size: 2.2rem; margin-bottom: 0.6rem; }
        .description { font-size: 0.95rem; }
        .feature-card p { display: none; }
        .content { gap: 1rem; padding: 1rem 2rem; }
    }
</style>
