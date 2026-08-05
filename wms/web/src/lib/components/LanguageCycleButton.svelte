<script>
    // Kiosk observer language switcher — the headline feature.
    // Three big tap targets: DE | EN | Other. DE/EN switch directly; "Other"
    // expands a plain list of every remaining available language below the
    // buttons. When an "other" language is active the third button shows its
    // code.
    import { onMount } from 'svelte';
    import { get } from 'svelte/store';
    import { authStore } from '$lib/stores/authStore';
    import { locale, availableLocales, setLocale, localeName, refreshAvailableLocales, t } from '$lib/i18n';

    let serverCycle = [];
    let open = false;

    onMount(async () => {
        // One fetch: merges customer-added langs into availableLocales AND gives
        // us the users-languages union for the display order.
        const token = get(authStore).token;
        const data = await refreshAvailableLocales(token);
        if (data?.success && Array.isArray(data.languages) && data.languages.length) {
            serverCycle = data.languages;
        }
    });

    // Only offer languages we actually have dictionaries for, keeping the
    // server's frequency order; fall back to the full available set.
    $: cycle = (() => {
        const src = serverCycle.length ? serverCycle : $availableLocales;
        const filtered = src.filter((c) => $availableLocales.includes(c));
        return filtered.length ? filtered : [...$availableLocales];
    })();

    // "Other" offers EVERY non-DE/EN dictionary we have: popular ones first
    // (server order), the long tail alphabetically.
    $: others = [...new Set([...cycle, ...$availableLocales])].filter(
        (c) => c !== 'de' && c !== 'en',
    );
    $: otherActive = $locale !== 'de' && $locale !== 'en';

    function pick(code) {
        open = false;
        setLocale(code, get(authStore).currentUser?.id ?? null);
    }
</script>

<div class="lang-switch" role="group" aria-label={$t('shell.change_language')}>
    <button type="button" class="lang-btn" class:active={$locale === 'de'} on:click={() => pick('de')}>
        DE
    </button>
    <button type="button" class="lang-btn" class:active={$locale === 'en'} on:click={() => pick('en')}>
        EN
    </button>
    {#if others.length}
        <button type="button" class="lang-btn other" class:active={otherActive} on:click={() => (open = !open)}>
            {#if otherActive}
                {$locale.toUpperCase()}
                <span class="sub">{$t('shell.lang_other')}</span>
            {:else}
                {$t('shell.lang_other')}
                <span class="sub">{open ? '▲' : '▼'}</span>
            {/if}
        </button>
    {/if}
</div>

{#if open}
    <div class="lang-list">
        {#each others as code (code)}
            <button type="button" class="lang-item" class:active={code === $locale} on:click={() => pick(code)}>
                <span class="code">{code.toUpperCase()}</span>
                <span class="name">{localeName(code)}</span>
            </button>
        {/each}
    </div>
{/if}

<style>
    .lang-switch {
        display: flex;
        gap: 6px;
        width: 100%;
    }

    .lang-btn {
        flex: 1;
        display: flex;
        flex-direction: column;
        align-items: center;
        justify-content: center;
        gap: 2px;
        /* Big tap targets for kiosk touchscreens */
        min-height: 64px;
        padding: 0.5rem 0.25rem;
        background: #2a2a2a;
        border: 1px solid #444;
        border-radius: 8px;
        color: #999;
        font-weight: 700;
        font-size: 1.05rem;
        letter-spacing: 0.5px;
        cursor: pointer;
        transition: all 0.15s;
    }

    .lang-btn:hover {
        background: #333;
        color: #ccc;
    }

    /* Active language: dark tone with a faint blue cast, not a bright pill */
    .lang-btn.active {
        background: #2c3140;
        border-color: rgba(74, 105, 189, 0.55);
        color: #e5eaf2;
    }

    .lang-btn .sub {
        font-size: 0.68rem;
        font-weight: 600;
        letter-spacing: 0.3px;
        color: #888;
        text-transform: none;
    }

    .lang-btn.active .sub {
        color: #9aa7c4;
    }

    /* Expanded picker: plain rows right below the buttons; the sidebar
       scrolls, so a long list just flows down. */
    .lang-list {
        display: flex;
        flex-direction: column;
        gap: 3px;
        margin-top: 6px;
    }

    .lang-item {
        display: flex;
        align-items: center;
        gap: 0.6rem;
        padding: 0.5rem 0.75rem;
        background: #232323;
        border: 1px solid #3a3a3a;
        border-radius: 6px;
        color: #b8bcc6;
        font-size: 0.9rem;
        cursor: pointer;
        text-align: left;
        transition: all 0.15s;
    }

    .lang-item:hover {
        background: #2e2e2e;
        color: #d5d9e2;
    }

    .lang-item.active {
        background: #2c3140;
        border-color: rgba(74, 105, 189, 0.55);
        color: #e5eaf2;
    }

    .lang-item .code {
        font-weight: 700;
        font-size: 0.8rem;
        letter-spacing: 0.5px;
        min-width: 2em;
        color: #8f97a5;
    }

    .lang-item.active .code {
        color: #b9c6e8;
    }
</style>
