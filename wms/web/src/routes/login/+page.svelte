<script>
    import { onMount } from 'svelte';
    import { authStore } from '$lib/stores/authStore';
    import { goto } from '$app/navigation';
    import { base } from '$app/paths';

    let email = '';
    let password = '';
    let error = '';
    let isLoading = false;

    let needsSetup = false;
    let setupEmail = '';
    let setupPassword = '';
    let pollInterval;
    let enableKiosk = false;

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
            error = 'Please fill in all fields';
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
                    body: JSON.stringify({ enabled: true })
                }).catch(() => {});
            }
            const pathBase = base || '/E';
            goto(`${pathBase}/dashboard`);
        } else {
            error = result.error || 'Login failed';
        }
        isLoading = false;
    }
</script>

<div class="login-page">
    <nav class="top-bar">
        <div class="top-bar-left">
            <span class="e-label">/E/</span>
            <span class="top-bar-brand">eckWMS</span>
        </div>
        <a class="github-link" href="https://github.com/niceguydave/eckwms" target="_blank" rel="noopener">
            <svg viewBox="0 0 16 16" fill="currentColor" width="20" height="20"><path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.013 8.013 0 0016 8c0-4.42-3.58-8-8-8z"/></svg>
        </a>
    </nav>

    <div class="login-container">
    <div class="login-card">
        <div class="logo">
            <h1>eckWMS</h1>
            {#if needsSetup}
                <span class="version setup-mode">First Run</span>
            {:else}
                <span class="version">Rust Edition</span>
            {/if}
        </div>

        {#if needsSetup}
            <div class="setup-banner">
                <div class="setup-title">Initial Setup</div>
                <p>No users exist yet. Use the temporary credentials below to log in, then create your admin account.</p>
                <div class="setup-creds">
                    <div class="cred-row">
                        <span class="cred-label">Email</span>
                        <span class="cred-value">{setupEmail}</span>
                    </div>
                    <div class="cred-row">
                        <span class="cred-label">Password</span>
                        <span class="cred-value mono">{setupPassword}</span>
                    </div>
                </div>
                <p class="setup-hint">This banner disappears once you create a real admin account.</p>
                {#if needsSetup}
                    <label class="kiosk-toggle">
                        <input type="checkbox" bind:checked={enableKiosk} />
                        <span>Enable Kiosk Auto-Login (observer mode for this device)</span>
                    </label>
                {/if}
            </div>
        {/if}

        <form on:submit|preventDefault={handleLogin}>
            <div class="form-group">
                <label for="email">Email</label>
                <input
                    type="text"
                    id="email"
                    bind:value={email}
                    placeholder="operator@eckwms.local"
                    disabled={isLoading}
                />
            </div>

            <div class="form-group">
                <label for="password">Password</label>
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
                {isLoading ? 'Authenticating...' : 'Login'}
            </button>
        </form>
    </div>
</div>
</div>

<style>
    .login-page {
        height: 100vh;
        display: flex;
        flex-direction: column;
    }

    .top-bar {
        display: flex;
        align-items: center;
        justify-content: space-between;
        padding: 0.6rem 1.2rem;
        background: #1a1a1a;
        border-bottom: 1px solid #333;
    }

    .top-bar-left {
        display: flex;
        align-items: center;
        gap: 0.5rem;
    }

    .e-label {
        font-size: 1.1rem;
        font-weight: 800;
        font-family: monospace;
        color: #e03c31;
        text-shadow: 0 0 10px rgba(224, 60, 49, 0.7);
    }

    .top-bar-brand {
        font-size: 1.1rem;
        font-weight: 700;
        color: #e0e0e0;
        letter-spacing: -0.5px;
    }

    .github-link {
        color: #888;
        transition: color 0.2s;
        display: flex;
        align-items: center;
    }

    .github-link:hover {
        color: #fff;
    }
    .login-container {
        flex: 1;
        display: flex;
        align-items: center;
        justify-content: center;
        background-color: #1e1e1e;
        background-image: radial-gradient(#2a2a2a 1px, transparent 1px);
        background-size: 20px 20px;
    }

    .login-card {
        background: #2c2c2e;
        padding: 2.5rem;
        border-radius: 8px;
        width: 100%;
        max-width: 420px;
        box-shadow: 0 10px 25px rgba(0,0,0,0.5);
        border: 1px solid #444;
    }

    .logo {
        text-align: center;
        margin-bottom: 2rem;
    }

    .logo h1 {
        margin: 0;
        color: #4a69bd;
        font-size: 2rem;
        font-weight: 700;
        letter-spacing: -1px;
    }

    .version {
        font-size: 0.8rem;
        color: #666;
        text-transform: uppercase;
        letter-spacing: 2px;
    }

    .setup-mode {
        color: #e8a838;
    }

    .setup-banner {
        background: rgba(232, 168, 56, 0.08);
        border: 1px solid rgba(232, 168, 56, 0.3);
        border-radius: 6px;
        padding: 1rem 1.2rem;
        margin-bottom: 1.5rem;
        font-size: 0.85rem;
        color: #ccc;
    }

    .setup-title {
        font-weight: 700;
        color: #e8a838;
        text-transform: uppercase;
        letter-spacing: 1px;
        font-size: 0.75rem;
        margin-bottom: 0.4rem;
    }

    .setup-banner p {
        margin: 0 0 0.75rem 0;
        line-height: 1.5;
    }

    .setup-creds {
        background: rgba(0,0,0,0.3);
        border-radius: 4px;
        padding: 0.6rem 0.8rem;
        margin-bottom: 0.75rem;
    }

    .cred-row {
        display: flex;
        gap: 0.75rem;
        align-items: baseline;
        padding: 0.2rem 0;
    }

    .cred-label {
        color: #888;
        min-width: 60px;
        font-size: 0.8rem;
    }

    .cred-value {
        color: #fff;
        font-size: 0.9rem;
    }

    .cred-value.mono {
        font-family: monospace;
        font-size: 1rem;
        color: #e8a838;
        letter-spacing: 1px;
    }

    .setup-hint {
        color: #666;
        font-size: 0.78rem;
        margin: 0;
    }

    .form-group {
        margin-bottom: 1.5rem;
    }

    label {
        display: block;
        margin-bottom: 0.5rem;
        color: #aaa;
        font-size: 0.9rem;
    }

    input {
        width: 100%;
        padding: 0.75rem;
        background: #1a1a1a;
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
        padding: 0.8rem;
        background: #4a69bd;
        color: white;
        border: none;
        border-radius: 4px;
        font-size: 1rem;
        font-weight: 600;
        cursor: pointer;
        transition: background 0.2s;
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
        padding: 0.75rem;
        border-radius: 4px;
        margin-bottom: 1.5rem;
        font-size: 0.9rem;
        text-align: center;
    }

    .kiosk-toggle {
        display: flex;
        align-items: center;
        gap: 0.5rem;
        margin-top: 0.75rem;
        cursor: pointer;
        font-size: 0.82rem;
        color: #aaa;
    }

    .kiosk-toggle input[type="checkbox"] {
        width: auto;
        accent-color: #4a69bd;
    }
</style>
