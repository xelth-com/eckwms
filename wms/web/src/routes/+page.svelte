<script>
    import { authStore } from '$lib/stores/authStore';
    import { onMount } from 'svelte';
    import { base } from '$app/paths';
    import { goto } from '$app/navigation';
    import { page } from '$app/stores';
    import { t } from '$lib/i18n';

    // Мы больше не делаем авто-редирект, чтобы пользователи могли прочитать о системе.
    // Состояние авторизации используется только для переключения кнопки Login/Dashboard.
    // EXCEPTION — kiosk/public-observer sessions (exhibition nodes with
    // ECK_PUBLIC_OBSERVER, or the local kiosk): those visitors should land in
    // the dashboard, not on the landing page. `?stay=1` keeps the page readable.
    $: if ($authStore.isKioskObserver && !$authStore.isLoading
           && !$page.url.searchParams.has('stay')) {
        goto(`${base || '/E'}/dashboard`);
    }
</script>

<div class="landing-page">
    <nav class="navbar">
        <div class="logo">
            <span class="e-label">/E/</span>
            eckWMS <span class="badge">RUST</span>
        </div>
        <div class="nav-links">
            <a href="https://github.com/xelth-com/eckwms" target="_blank" rel="noreferrer" class="github-link">
                GitHub
            </a>
        </div>
    </nav>

    <main class="hero">
        <div class="hero-content">
            <h1>{$t('shell.landing_headline')} <br><span class="accent">{$t('shell.landing_headline_accent')}</span></h1>

            <p class="description">
                {@html $t('shell.landing_description')}
            </p>

            <div class="cta-group">
                {#if $authStore.isLoading}
                    <button class="btn primary loading">{$t('shell.loading')}</button>
                {:else if $authStore.isAuthenticated}
                    <a href="{base}/dashboard" class="btn primary">
                        {$t('shell.landing_open_dashboard')} &rarr;
                    </a>
                {:else}
                    <a href="{base}/login" class="btn primary">
                        {$t('shell.landing_sign_in')}
                    </a>
                {/if}
                <a href="https://github.com/xelth-com/eckwms" target="_blank" rel="noreferrer" class="btn secondary">
                    {$t('shell.landing_view_source')}
                </a>
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

    <footer>
        <p>{$t('shell.landing_footer', { year: new Date().getFullYear() })}</p>
    </footer>
</div>

<style>
    .landing-page {
        min-height: 100vh;
        background-color: #121212;
        color: #e0e0e0;
        display: flex;
        flex-direction: column;
        font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Oxygen, Ubuntu, Cantarell, 'Noto Sans KR Variable', 'Noto Sans JP Variable', 'Noto Sans SC Variable', sans-serif;
    }

    .navbar {
        display: flex;
        justify-content: space-between;
        align-items: center;
        padding: 1.5rem 2rem;
        background: rgba(30, 30, 30, 0.5);
        backdrop-filter: blur(10px);
        border-bottom: 1px solid #333;
    }

    .logo {
        font-size: 1.5rem;
        font-weight: 800;
        color: #fff;
        letter-spacing: -0.5px;
    }

    .e-label {
        font-size: 1.1rem;
        font-weight: 800;
        font-family: monospace;
        color: #e03c31;
        text-shadow: 0 0 10px rgba(224, 60, 49, 0.7);
        margin-right: 4px;
        vertical-align: middle;
    }

    .badge {
        background: linear-gradient(135deg, #e03c31, #ff6b35);
        font-size: 0.7rem;
        padding: 2px 8px;
        border-radius: 4px;
        vertical-align: middle;
        margin-left: 5px;
        font-weight: 700;
        letter-spacing: 1px;
        box-shadow: 0 0 12px rgba(224, 60, 49, 0.4);
    }

    .github-link {
        color: #aaa;
        text-decoration: none;
        font-weight: 500;
        transition: color 0.2s;
    }
    .github-link:hover { color: #fff; }

    .hero {
        flex: 1;
        display: flex;
        flex-direction: column;
        align-items: center;
        justify-content: center;
        padding: 4rem 2rem;
        text-align: center;
        background-image: radial-gradient(#2a2a2a 1px, transparent 1px);
        background-size: 30px 30px;
    }

    h1 {
        font-size: 3.5rem;
        line-height: 1.1;
        margin-bottom: 1.5rem;
        background: linear-gradient(to right, #fff, #aaa);
        -webkit-background-clip: text;
        -webkit-text-fill-color: transparent;
    }

    .accent {
        color: #e03c31;
        -webkit-text-fill-color: #e03c31;
    }

    .description {
        max-width: 600px;
        font-size: 1.2rem;
        color: #888;
        line-height: 1.6;
        margin-bottom: 3rem;
    }

    .cta-group {
        display: flex;
        gap: 1rem;
        margin-bottom: 5rem;
        flex-wrap: wrap;
        justify-content: center;
    }

    .btn {
        padding: 1rem 2rem;
        border-radius: 8px;
        font-weight: 600;
        text-decoration: none;
        transition: transform 0.2s, opacity 0.2s;
        font-size: 1.1rem;
    }

    .btn:hover {
        transform: translateY(-2px);
    }

    .btn.primary {
        background: #4a69bd;
        color: white;
        box-shadow: 0 4px 15px rgba(74, 105, 189, 0.4);
    }

    .btn.secondary {
        background: #2a2a2a;
        color: #fff;
        border: 1px solid #444;
    }

    .features-grid {
        display: grid;
        grid-template-columns: repeat(auto-fit, minmax(250px, 1fr));
        gap: 2rem;
        max-width: 1200px;
        width: 100%;
        text-align: left;
    }

    .feature-card {
        background: #1e1e1e;
        border: 1px solid #333;
        padding: 2rem;
        border-radius: 12px;
        transition: border-color 0.2s;
    }

    .feature-card:hover {
        border-color: #4a69bd;
    }

    .feature-card h3 {
        margin-top: 0;
        color: #e0e0e0;
        margin-bottom: 0.5rem;
    }

    .feature-card p {
        color: #888;
        font-size: 0.95rem;
        line-height: 1.5;
        margin: 0;
    }

    footer {
        padding: 2rem;
        text-align: center;
        color: #555;
        font-size: 0.9rem;
        border-top: 1px solid #222;
    }

    @media (max-width: 768px) {
        h1 { font-size: 2.5rem; }
        .hero { padding: 2rem 1rem; }
    }
</style>
