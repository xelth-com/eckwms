<script>
    import { onMount } from 'svelte';
    import { api } from '$lib/api';
    import { toastStore } from '$lib/stores/toastStore.js';
    import { t, tr } from '$lib/i18n';

    let loading = false;
    let dumpData = [];

    // --- Batch CSV Enrichment state ---
    let csvFile = null;
    let enrichLoading = false;
    let enrichResults = [];
    let enrichNote = '';

    // Prompt builder state
    let systemPrompt = `You are an expert technical support analyst for the company devices.
I will provide you with a log of our recent support tickets.
Please analyze these conversations and identify:
1. The most common hardware issues.
2. The standard solutions we provided that successfully resolved the issues.
3. Generate a step-by-step troubleshooting guide for the top 3 most frequent problems.

Here is the data:
---
`;
    let filterStatus = 'Closed';

    async function fetchDump() {
        loading = true;
        try {
            const res = await api.get('/api/analysis/support-dump');
            dumpData = res || [];
            toastStore.add(tr('analysis.loaded_tickets', { count: dumpData.length }), 'success');
        } catch (e) {
            toastStore.add(tr('analysis.fetch_failed', { error: e.message }), 'error');
        } finally {
            loading = false;
        }
    }

    function onCsvSelected(e) {
        const files = e.target.files;
        csvFile = files && files.length > 0 ? files[0] : null;
        enrichResults = [];
        enrichNote = '';
    }

    async function enrichCsv() {
        if (!csvFile) {
            toastStore.add(tr('analysis.select_csv'), 'warning');
            return;
        }
        enrichLoading = true;
        enrichResults = [];
        enrichNote = '';
        try {
            const token = localStorage.getItem('auth_token');
            const fd = new FormData();
            fd.append('file', csvFile);
            const res = await fetch('/api/ai/enrich-csv', {
                method: 'POST',
                headers: { Authorization: `Bearer ${token}` },
                body: fd,
            });
            if (!res.ok) {
                const errText = await res.text();
                throw new Error(errText || tr('analysis.request_failed', { status: res.status }));
            }
            const data = await res.json();
            enrichResults = data.results || [];
            enrichNote = data.note || '';
            toastStore.add(tr('analysis.enriched_rows', { count: enrichResults.length }), 'success');
        } catch (err) {
            toastStore.add(tr('analysis.enrichment_failed', { error: err.message }), 'error');
        } finally {
            enrichLoading = false;
        }
    }

    async function copyPromptToAI() {
        if (dumpData.length === 0) {
            toastStore.add(tr('analysis.fetch_first'), 'warning');
            return;
        }

        let filtered = dumpData;
        if (filterStatus !== 'All') {
            filtered = dumpData.filter(t => t.status.toLowerCase() === filterStatus.toLowerCase());
        }

        if (filtered.length === 0) {
            toastStore.add(tr('analysis.no_tickets_status', { status: filterStatus }), 'warning');
            return;
        }

        let compiledText = systemPrompt + "\n";
        filtered.forEach(t => {
            compiledText += `\n[TICKET #${t.ticket_number}] Status: ${t.status}\nSubject: ${t.subject}\nConversation:\n${t.text_content}\n`;
            compiledText += `--------------------------------------------------\n`;
        });

        try {
            await navigator.clipboard.writeText(compiledText);
            toastStore.add(tr('analysis.copied_prompt', { count: filtered.length }), 'success');
        } catch (err) {
            toastStore.add(tr('analysis.copy_failed', { error: err.message }), 'error');
        }
    }
</script>

<div class="analysis-page">
    <header>
        <h1>{$t('analysis.page_title')}</h1>
        <p class="subtitle">{$t('analysis.subtitle')}</p>
    </header>

    <div class="grid">
        <div class="card">
            <div class="card-header">
                <h2>{$t('analysis.extractor_title')}</h2>
                <span class="badge">{$t('analysis.badge_ready')}</span>
            </div>
            <p class="desc">
                {$t('analysis.extractor_desc')}
            </p>

            <div class="controls">
                <button class="btn secondary" on:click={fetchDump} disabled={loading}>
                    {loading ? $t('analysis.fetching_db') : $t('analysis.fetch_db')}
                </button>
                <span class="record-count">
                    {#if dumpData.length > 0}
                        {$t('analysis.records_loaded', { count: dumpData.length })}
                    {/if}
                </span>
            </div>

            <div class="prompt-builder" class:disabled={dumpData.length === 0}>
                <label>{$t('analysis.filter_label')}</label>
                <select bind:value={filterStatus}>
                    <option value="Closed">{$t('analysis.filter_closed')}</option>
                    <option value="All">{$t('analysis.filter_all')}</option>
                    <option value="Open">{$t('analysis.filter_open')}</option>
                </select>

                <label>{$t('analysis.system_prompt_label')}</label>
                <textarea bind:value={systemPrompt} rows="8"></textarea>

                <button class="btn primary" on:click={copyPromptToAI} disabled={dumpData.length === 0}>
                    {$t('analysis.copy_prompt_btn')}
                </button>
            </div>
        </div>

        <div class="card">
            <div class="card-header">
                <h2>{$t('analysis.enrich_title')}</h2>
                <span class="badge">{$t('analysis.badge_ready')}</span>
            </div>
            <p class="desc">
                {$t('analysis.enrich_desc_1')}
                <code>search_database</code>{$t('analysis.enrich_desc_2')}
                <code>order</code> {$t('analysis.enrich_desc_3')} <code>document</code> {$t('analysis.enrich_desc_4')}
            </p>

            <div class="controls">
                <input type="file" accept=".csv,text/csv" on:change={onCsvSelected} disabled={enrichLoading} />
                <button class="btn primary" on:click={enrichCsv} disabled={enrichLoading || !csvFile}>
                    {enrichLoading ? $t('analysis.enriching') : $t('analysis.enrich_btn')}
                </button>
            </div>

            {#if enrichNote}
                <p class="record-count">{enrichNote}</p>
            {/if}

            {#if enrichResults.length > 0}
                <div class="enrich-table-wrap">
                    <table class="enrich-table">
                        <thead>
                            <tr>
                                <th>{$t('analysis.th_raw_row')}</th>
                                <th>{$t('analysis.th_order_number')}</th>
                                <th>{$t('analysis.th_customer')}</th>
                                <th>{$t('analysis.th_model')}</th>
                                <th>{$t('analysis.th_issue')}</th>
                                <th>{$t('analysis.th_conf')}</th>
                            </tr>
                        </thead>
                        <tbody>
                            {#each enrichResults as r}
                                <tr class:low-conf={(r.confidence_score ?? 0) < 0.4}>
                                    <td class="mono">{r.original_line ?? ''}</td>
                                    <td>{r.matched_order_number ?? '—'}</td>
                                    <td>{r.matched_customer ?? '—'}</td>
                                    <td>{r.device_model ?? '—'}</td>
                                    <td>{r.issue_notes ?? '—'}</td>
                                    <td>{r.confidence_score != null ? r.confidence_score.toFixed(2) : '—'}</td>
                                </tr>
                            {/each}
                        </tbody>
                    </table>
                </div>
            {/if}
        </div>

        <div class="card wip">
            <div class="card-header">
                <h2>{$t('analysis.rag_title')}</h2>
                <span class="badge wip-badge">{$t('analysis.badge_wip')}</span>
            </div>
            <p class="desc">
                {$t('analysis.rag_desc_1')} <code>rig-core</code>{$t('analysis.rag_desc_2')}
            </p>
            <div class="placeholder-box">
                {$t('analysis.rag_placeholder')}
            </div>
        </div>
    </div>
</div>

<style>
    .analysis-page { padding: 0 0 2rem 0; }
    header { margin-bottom: 2rem; }
    h1 { font-size: 1.8rem; color: #fff; margin: 0 0 0.5rem 0; }
    .subtitle { color: #888; margin: 0; font-size: 1rem; }

    .grid { display: grid; grid-template-columns: 1fr 1fr; gap: 1.5rem; }

    .card { background: #1e1e1e; border: 1px solid #333; border-radius: 8px; padding: 1.5rem; display: flex; flex-direction: column; gap: 1rem; }
    .card.wip { opacity: 0.7; border-style: dashed; }

    .card-header { display: flex; justify-content: space-between; align-items: center; }
    .card-header h2 { margin: 0; color: #e0e0e0; font-size: 1.2rem; }

    .badge { background: #1a3a1a; color: #4ade80; border: 1px solid #22c55e; padding: 0.2rem 0.5rem; border-radius: 4px; font-size: 0.7rem; font-weight: bold; text-transform: uppercase; }
    .wip-badge { background: #3a2a0a; color: #fbbf24; border-color: #f59e0b; }

    .desc { color: #aaa; font-size: 0.9rem; line-height: 1.5; margin: 0; }

    .controls { display: flex; align-items: center; gap: 1rem; padding-bottom: 1rem; border-bottom: 1px solid #333; }
    .record-count { color: #4a69bd; font-weight: 600; font-size: 0.9rem; }

    .prompt-builder { display: flex; flex-direction: column; gap: 0.75rem; transition: opacity 0.3s; }
    .prompt-builder.disabled { opacity: 0.4; pointer-events: none; }
    .prompt-builder label { color: #ccc; font-size: 0.85rem; font-weight: 600; margin-top: 0.5rem; }
    .prompt-builder select, .prompt-builder textarea { background: #121212; border: 1px solid #444; color: #fff; padding: 0.75rem; border-radius: 4px; font-family: inherit; font-size: 0.9rem; width: 100%; box-sizing: border-box; }
    .prompt-builder select:focus, .prompt-builder textarea:focus { outline: none; border-color: #4a69bd; }

    .btn { padding: 0.75rem 1.5rem; border-radius: 6px; font-weight: 600; cursor: pointer; border: none; transition: all 0.2s; font-size: 0.9rem; }
    .btn.primary { background: #4a69bd; color: white; }
    .btn.primary:hover:not(:disabled) { background: #3a59ad; }
    .btn.secondary { background: #2a2a2a; color: #ccc; border: 1px solid #444; }
    .btn.secondary:hover:not(:disabled) { background: #3a3a3a; color: #fff; }
    .btn:disabled { opacity: 0.5; cursor: not-allowed; }

    .placeholder-box { background: #121212; border: 1px dashed #444; border-radius: 4px; padding: 2rem; text-align: center; color: #666; font-style: italic; margin-top: auto; }

    .controls input[type="file"] { color: #ccc; font-size: 0.85rem; }
    .enrich-table-wrap { max-height: 420px; overflow: auto; border: 1px solid #333; border-radius: 4px; }
    .enrich-table { width: 100%; border-collapse: collapse; font-size: 0.8rem; }
    .enrich-table th, .enrich-table td { padding: 0.4rem 0.6rem; border-bottom: 1px solid #2a2a2a; text-align: left; vertical-align: top; color: #ddd; }
    .enrich-table th { background: #161616; color: #9ab; position: sticky; top: 0; }
    .enrich-table tr.low-conf td { color: #999; }
    .enrich-table .mono { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; color: #888; max-width: 280px; word-break: break-all; }

    @media (max-width: 900px) { .grid { grid-template-columns: 1fr; } }
</style>
