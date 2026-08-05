<script>
    import { onMount } from 'svelte';
    import { t } from '$lib/i18n';

    // The paid POS (ecKasse) isn't active on this node — this is the upsell
    // surface reached from the golden button at the bottom of the sidebar.
    const features = [
        { icon: '🧾', key: 'f_register' },
        { icon: '📦', key: 'f_warehouse' },
        { icon: '🤖', key: 'f_gemini' },
        { icon: '🍽️', key: 'f_tables' },
        { icon: '🔒', key: 'f_tse' },
        { icon: '📊', key: 'f_dsfinvk' },
    ];

    // License state from GET /api/pos/status — adapts the hero copy. The
    // pricing/feature cards below stay the same in every state.
    let posLicense = 'unlicensed';
    let posConfigured = false;
    let posLicenseReason = '';

    async function loadPosStatus() {
        try {
            const res = await fetch('/api/pos/status');
            if (res.ok) {
                const status = await res.json();
                posLicense = typeof status.license === 'string' ? status.license : 'unlicensed';
                posConfigured = status.configured === true;
                posLicenseReason = typeof status.license_reason === 'string' ? status.license_reason : '';
            }
        } catch { /* older server without the endpoint — keep the default upsell copy */ }
    }

    onMount(loadPosStatus);

    $: configuredUnlicensed = posLicense === 'unlicensed' && posConfigured;
    $: licensedNotConfigured = posLicense === 'licensed' && !posConfigured;
</script>

<div class="promo">
    <header class="hero">
        <div class="badge">💶 ecKasse</div>
        {#if configuredUnlicensed}
            <h1>{$t('pos_promo.configured_unlicensed_headline')}</h1>
            <p class="sub">{$t('pos_promo.configured_unlicensed_subhead')}</p>
            {#if posLicenseReason}
                <p class="reason-line">{$t('pos_promo.license_reason_label')} <code>{posLicenseReason}</code></p>
            {/if}
            <a class="cta primary contact-cta" href="{`/E/dashboard/ai`}">
                {$t('pos_promo.cta_contact')}
            </a>
        {:else}
            <h1>{$t('pos_promo.headline')}</h1>
            <p class="sub">{$t('pos_promo.subhead')}</p>
            {#if licensedNotConfigured}
                <p class="licensed-note">{$t('pos_promo.licensed_not_configured_note')}</p>
            {/if}
        {/if}
    </header>

    <section class="cards">
        {#each features as f}
            <div class="card">
                <span class="ico">{f.icon}</span>
                <h3>{$t(`pos_promo.${f.key}_title`)}</h3>
                <p>{$t(`pos_promo.${f.key}_desc`)}</p>
            </div>
        {/each}
    </section>

    <section class="pricing">
        <div class="price-tag">
            <span class="label">{$t('pos_promo.price_label')}</span>
            <span class="hint">{$t('pos_promo.price_hint')}</span>
        </div>
        <div class="ctas">
            <!-- External demo/showroom. rel=noopener; opens the public site. -->
            <a class="cta primary" href="https://eckasse.com" target="_blank" rel="noopener noreferrer">
                {$t('pos_promo.cta_demo')}
            </a>
            <a class="cta ghost" href="{`/E/dashboard/ai`}">
                {$t('pos_promo.cta_activate')}
            </a>
        </div>
        <p class="note">{$t('pos_promo.activate_note')}</p>
    </section>
</div>

<style>
    .promo { max-width: 960px; margin: 0 auto; padding: 2rem 1.5rem 4rem; color: #e5e5ea; }
    .hero { text-align: center; margin-bottom: 2.5rem; }
    .badge {
        display: inline-block; font-weight: 600; color: #f5b942;
        background: rgba(245, 185, 66, 0.1); border: 1px solid rgba(245, 185, 66, 0.35);
        padding: 0.35rem 0.9rem; border-radius: 999px; margin-bottom: 1rem;
    }
    .hero h1 { font-size: 2.2rem; margin: 0 0 0.6rem; }
    .sub { color: #aaa; font-size: 1.05rem; max-width: 640px; margin: 0 auto; line-height: 1.5; }
    .reason-line { color: #888; font-size: 0.85rem; margin: 0.6rem auto 0; max-width: 640px; }
    .reason-line code { background: #1e1e1e; border: 1px solid #333; border-radius: 4px; padding: 0.1rem 0.4rem; color: #f5b942; }
    .licensed-note { color: #a3bffa; font-size: 0.95rem; margin: 0.8rem auto 0; max-width: 640px; }
    .contact-cta { display: inline-block; margin-top: 1.2rem; }

    .cards {
        display: grid; grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
        gap: 1rem; margin-bottom: 2.5rem;
    }
    .card {
        background: #1e1e1e; border: 1px solid #333; border-radius: 12px; padding: 1.2rem;
    }
    .card .ico { font-size: 1.6rem; }
    .card h3 { margin: 0.5rem 0 0.4rem; font-size: 1.05rem; color: #fff; }
    .card p { margin: 0; color: #999; font-size: 0.9rem; line-height: 1.45; }

    .pricing { text-align: center; }
    .price-tag { margin-bottom: 1.4rem; }
    .price-tag .label { display: block; font-size: 1.3rem; font-weight: 700; color: #f5b942; }
    .price-tag .hint { color: #888; font-size: 0.9rem; }
    .ctas { display: flex; gap: 0.8rem; justify-content: center; flex-wrap: wrap; }
    .cta {
        padding: 0.75rem 1.6rem; border-radius: 8px; text-decoration: none;
        font-weight: 600; transition: all 0.2s;
    }
    .cta.primary { background: #f5b942; color: #1a1a1a; }
    .cta.primary:hover { background: #ffd27a; }
    .cta.ghost { border: 1px solid #4a69bd; color: #a3bffa; }
    .cta.ghost:hover { background: rgba(74, 105, 189, 0.15); }
    .note { color: #777; font-size: 0.82rem; margin-top: 1rem; }
</style>
