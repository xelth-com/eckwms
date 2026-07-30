<script>
    // Kiosk observer language cycle — the headline feature.
    // One big tap target that shows the WHOLE cycle of languages as uppercase
    // codes (DE → KO → EN → RU) with the current locale highlighted. Tapping
    // advances to the next language. The cycle is the union of all users'
    // languages (frequency-ordered) served by GET /api/i18n/languages; on any
    // failure we fall back to the dictionaries the UI actually ships.
    import { onMount } from 'svelte';
    import { get } from 'svelte/store';
    import { authStore } from '$lib/stores/authStore';
    import { locale, availableLocales, cycleLocale, refreshAvailableLocales, t } from '$lib/i18n';

    let serverCycle = [];

    onMount(async () => {
        // One fetch: merges customer-added langs into availableLocales AND gives
        // us the users-languages union for the cycle order.
        const token = get(authStore).token;
        const data = await refreshAvailableLocales(token);
        if (data?.success && Array.isArray(data.languages) && data.languages.length) {
            serverCycle = data.languages;
        }
    });

    // Only cycle through languages we actually have dictionaries for, keeping the
    // server's frequency order; fall back to the full available set.
    $: cycle = (() => {
        const src = serverCycle.length ? serverCycle : $availableLocales;
        const filtered = src.filter((c) => $availableLocales.includes(c));
        return filtered.length ? filtered : [...$availableLocales];
    })();

    function advance() {
        cycleLocale(cycle, get(authStore).currentUser?.id ?? null);
    }
</script>

<button
    class="lang-cycle"
    type="button"
    on:click={advance}
    aria-label={$t('shell.change_language')}
    title={$t('shell.change_language')}
>
    {#each cycle as code, i}
        <span class="code" class:active={code === $locale}>{code.toUpperCase()}</span>
        {#if i < cycle.length - 1}<span class="arrow" aria-hidden="true">→</span>{/if}
    {/each}
</button>

<style>
    .lang-cycle {
        display: flex;
        align-items: center;
        justify-content: center;
        flex-wrap: wrap;
        gap: 0.4rem;
        width: 100%;
        /* Big tap target for kiosk touchscreens */
        min-height: 56px;
        padding: 0.75rem 1rem;
        background: #2a2a2a;
        border: 1px solid #444;
        border-radius: 8px;
        cursor: pointer;
        transition: all 0.2s;
    }

    .lang-cycle:hover {
        background: #333;
        border-color: #4a69bd;
    }

    .code {
        font-weight: 700;
        font-size: 1rem;
        letter-spacing: 0.5px;
        color: #888;
        padding: 0.2rem 0.55rem;
        border-radius: 6px;
        transition: all 0.15s;
    }

    /* Current locale: accent pill + bold */
    .code.active {
        color: #fff;
        background: #4a69bd;
        box-shadow: 0 0 10px rgba(74, 105, 189, 0.4);
    }

    .arrow {
        color: #555;
        font-size: 0.9rem;
    }
</style>
