<script>
    // Embedded marketing site (exhibition nodes only): renders the external
    // cover site inside the dashboard content pane. The layout switches the
    // pane to full-bleed for /dashboard/site/*; ?embed=1 tells the site to
    // drop its own chrome and force the dark theme.
    import { onMount } from "svelte";
    import { page } from "$app/stores";
    import { locale } from "$lib/i18n";

    let siteUrl = "";
    onMount(async () => {
        try {
            const res = await fetch("/api/pos/status");
            if (res.ok) {
                const status = await res.json();
                siteUrl = typeof status.nav_site_url === "string" ? status.nav_site_url : "";
            }
        } catch {
            // stock node without the endpoint — leave blank
        }
    });

    $: sitePath = $page.params.path || "";
    $: src = siteUrl
        ? `${siteUrl}/${sitePath}?embed=1&lang=${$locale}${$page.url.hash || ""}`
        : "";
</script>

{#if src}
    <iframe class="site-frame" {src} title="9eck.com"></iframe>
{/if}

<style>
    .site-frame {
        position: absolute;
        inset: 0;
        width: 100%;
        height: 100%;
        border: 0;
        display: block;
        background: #121212;
    }
</style>
