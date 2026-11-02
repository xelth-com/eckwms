<script context="module">
    // Authenticated <img>. `GET /api/files/:id` sits behind auth_middleware
    // (wms/src/middleware/auth.rs), which only accepts a Bearer token in the
    // Authorization header — a plain <img src> gets a 401. This component
    // fetches the bytes with the token, turns them into an object URL and
    // renders that.
    //
    // A module-level cache keyed by URL means the same thumbnail shown many
    // times in a list is fetched once. It is a small LRU (MAX entries); a
    // cached object URL is never revoked while it stays in the map (live <img>
    // elements point at it), only when evicted. Callers that need the object
    // URL directly (e.g. a download link, which can't carry a header) use
    // `authedObjectURL`.
    import { get } from 'svelte/store';
    import { authStore } from '$lib/stores/authStore';

    const CACHE = new Map(); // url -> Promise<objectURL>
    const MAX = 200;

    function tokenOf() {
        const s = get(authStore);
        return (
            s?.token ||
            (typeof localStorage !== 'undefined' && localStorage.getItem('auth_token')) ||
            null
        );
    }

    export function authedObjectURL(url) {
        if (CACHE.has(url)) {
            // LRU bump: move to the most-recent end.
            const cached = CACHE.get(url);
            CACHE.delete(url);
            CACHE.set(url, cached);
            return cached;
        }

        const p = (async () => {
            const token = tokenOf();
            const resp = await fetch(url, {
                headers: token ? { Authorization: `Bearer ${token}` } : {},
            });
            if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
            const blob = await resp.blob();
            return URL.createObjectURL(blob);
        })();

        CACHE.set(url, p);
        // A failed fetch must not stick in the cache — drop it so a later mount
        // can retry (and so a transient 401 before the token lands recovers).
        p.catch(() => {
            if (CACHE.get(url) === p) CACHE.delete(url);
        });

        // Evict the oldest entry past the cap and revoke its object URL.
        if (CACHE.size > MAX) {
            const oldestKey = CACHE.keys().next().value;
            const oldest = CACHE.get(oldestKey);
            CACHE.delete(oldestKey);
            Promise.resolve(oldest)
                .then((u) => { try { URL.revokeObjectURL(u); } catch { /* already gone */ } })
                .catch(() => { /* never resolved — nothing to revoke */ });
        }

        return p;
    }
</script>

<script>
    export let src;
    export let alt = '';
    export let style = '';
    let className = '';
    export { className as class };

    let objUrl = null;
    let failed = false;
    let reqSrc = null; // the src the current load is for (guards out-of-order resolves)

    async function load(s) {
        try {
            const url = await authedObjectURL(s);
            if (s === reqSrc) { objUrl = url; failed = false; }
        } catch {
            if (s === reqSrc) { objUrl = null; failed = true; }
        }
    }

    // React to src changes (SPA nav / list re-render reusing the instance).
    $: if (src && src !== reqSrc) {
        reqSrc = src;
        objUrl = null;
        failed = false;
        load(src);
    }
</script>

{#if objUrl}
    <img src={objUrl} {alt} class={className} {style} />
{:else}
    <!-- Loading or failed: show the caller's fallback (glyph/placeholder). -->
    <slot name="fallback" {failed} />
{/if}
