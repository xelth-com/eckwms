<script>
    import { authStore } from '$lib/stores/authStore';
    import { toastStore } from '$lib/stores/toastStore';
    import { browser } from '$app/environment';
    import { t, tr } from '$lib/i18n';

    let platform = 'windows'; // 'windows' | 'linux'
    let tier = 'agent'; // 'agent' | 'master'
    let configOnly = false;
    let downloading = false;
    let copied = false;

    // Origin used for the direct-connection snippet — computed client-side
    // only (no SSR window access).
    $: origin = browser ? window.location.origin : '';
    $: directCommand = `claude mcp add --transport http 9eck-wms ${origin}/mcp --header "Authorization: Bearer <token>"`;

    async function downloadBundle() {
        downloading = true;
        try {
            const params = new URLSearchParams({ platform, tier });
            if (configOnly) params.set('config_only', '1');
            const res = await fetch(`/api/admin/mcp-connector?${params.toString()}`, {
                headers: { Authorization: `Bearer ${$authStore.token}` },
            });

            if (!res.ok) {
                const err = await res.json().catch(() => ({}));
                throw new Error(err.error || `Request failed: ${res.status}`);
            }

            const blob = await res.blob();
            const disposition = res.headers.get('Content-Disposition') || '';
            const match = disposition.match(/filename="?([^"; ]+)"?/i);
            const filename = match ? match[1] : `9eck-mcp-connector-${platform}.zip`;

            const url = window.URL.createObjectURL(blob);
            const a = document.createElement('a');
            a.href = url;
            a.download = filename;
            document.body.appendChild(a);
            a.click();
            window.URL.revokeObjectURL(url);
            document.body.removeChild(a);

            toastStore.add(tr('connector.toast_download_success'), 'success');
        } catch (e) {
            toastStore.add(tr('connector.toast_download_failed', { error: e.message }), 'error');
        } finally {
            downloading = false;
        }
    }

    function copyCommand() {
        navigator.clipboard.writeText(directCommand).then(() => {
            copied = true;
            setTimeout(() => (copied = false), 2000);
        });
    }
</script>

<div class="page">
    <header>
        <h1>{$t('connector.title')}</h1>
    </header>

    <p class="lead">{$t('connector.description')}</p>

    <section class="card">
        <div class="form-row">
            <div class="form-group">
                <label for="platform">{$t('connector.f_platform')}</label>
                <select id="platform" bind:value={platform}>
                    <option value="windows">{$t('connector.platform_windows')}</option>
                    <option value="linux">{$t('connector.platform_linux')}</option>
                </select>
            </div>
            <div class="form-group">
                <label for="tier">{$t('connector.f_tier')}</label>
                <select id="tier" bind:value={tier}>
                    <option value="agent">{$t('connector.tier_agent')}</option>
                    <option value="master">{$t('connector.tier_master')}</option>
                </select>
            </div>
        </div>

        <div class="form-check">
            <input type="checkbox" id="configOnly" bind:checked={configOnly} />
            <label for="configOnly">{$t('connector.f_config_only')}</label>
        </div>

        <button class="btn primary" on:click={downloadBundle} disabled={downloading}>
            {downloading ? $t('connector.downloading') : $t('connector.btn_download')}
        </button>
    </section>

    <section class="card direct">
        <h2>{$t('connector.direct_title')}</h2>
        <p class="lead">{$t('connector.direct_desc')}</p>
        <div class="snippet">
            <code>{directCommand}</code>
            <button class="btn-icon copy" on:click={copyCommand} title={$t('connector.copy')}>
                {copied ? $t('connector.copied') : $t('connector.copy')}
            </button>
        </div>
        <p class="note">{$t('connector.direct_token_note')}</p>
    </section>
</div>

<style>
    .page { padding: 2rem; max-width: 900px; margin: 0 auto; }
    header { margin-bottom: 0.5rem; }
    h1 { color: #fff; margin: 0; font-size: 1.5rem; }
    h2 { color: #fff; margin: 0 0 0.5rem; font-size: 1.1rem; }

    .lead { color: #999; margin: 0 0 1.5rem; line-height: 1.5; }

    .card {
        background: #1e1e1e; border: 1px solid #333; border-radius: 8px;
        padding: 1.5rem; margin-bottom: 1.5rem;
    }
    .card.direct .lead { margin-bottom: 1rem; }

    .form-row { display: grid; grid-template-columns: 1fr 1fr; gap: 1rem; }
    .form-group { margin-bottom: 1rem; }
    .form-group label { display: block; color: #aaa; margin-bottom: 0.3rem; font-size: 0.85rem; }
    .form-group select {
        width: 100%; padding: 0.7rem; background: #121212; border: 1px solid #444;
        color: #fff; border-radius: 6px; box-sizing: border-box; font-size: 0.9rem;
    }
    .form-group select:focus { border-color: #4a69bd; outline: none; }

    .form-check { display: flex; align-items: center; gap: 0.5rem; margin: 0.5rem 0 1.5rem; }
    .form-check label { color: #ccc; cursor: pointer; font-size: 0.9rem; }

    .btn { padding: 0.6rem 1.2rem; border-radius: 6px; border: none; font-weight: 600; cursor: pointer; font-size: 0.9rem; }
    .btn.primary { background: #4a69bd; color: white; }
    .btn.primary:hover { background: #3c5aa6; }
    .btn.primary:disabled { opacity: 0.6; cursor: not-allowed; }

    .snippet {
        display: flex; align-items: center; gap: 0.75rem;
        background: #121212; border: 1px solid #444; border-radius: 6px;
        padding: 0.8rem 1rem; overflow-x: auto;
    }
    .snippet code {
        flex: 1; color: #a3bffa; font-family: monospace; font-size: 0.85rem;
        white-space: pre; word-break: break-all;
    }
    .btn-icon.copy {
        background: #2a2a2a; color: #ccc; border: 1px solid #444; border-radius: 4px;
        padding: 0.4rem 0.8rem; cursor: pointer; font-size: 0.8rem; white-space: nowrap;
    }
    .btn-icon.copy:hover { background: #333; color: #fff; }

    .note { color: #666; font-size: 0.8rem; margin: 0.75rem 0 0; }
</style>
