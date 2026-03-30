<script>
    import { onMount } from 'svelte';
    import { api } from '$lib/api';
    import { toastStore } from '$lib/stores/toastStore.js';

    let loading = false;
    let dumpData = [];

    // Prompt builder state
    let systemPrompt = `You are an expert technical support analyst for InBody devices.
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
            toastStore.add(`Loaded ${dumpData.length} tickets for analysis`, 'success');
        } catch (e) {
            toastStore.add('Failed to fetch data: ' + e.message, 'error');
        } finally {
            loading = false;
        }
    }

    async function copyPromptToAI() {
        if (dumpData.length === 0) {
            toastStore.add('Please fetch data first', 'warning');
            return;
        }

        let filtered = dumpData;
        if (filterStatus !== 'All') {
            filtered = dumpData.filter(t => t.status.toLowerCase() === filterStatus.toLowerCase());
        }

        if (filtered.length === 0) {
            toastStore.add(`No tickets found with status: ${filterStatus}`, 'warning');
            return;
        }

        let compiledText = systemPrompt + "\n";
        filtered.forEach(t => {
            compiledText += `\n[TICKET #${t.ticket_number}] Status: ${t.status}\nSubject: ${t.subject}\nConversation:\n${t.text_content}\n`;
            compiledText += `--------------------------------------------------\n`;
        });

        try {
            await navigator.clipboard.writeText(compiledText);
            toastStore.add(`Copied prompt with ${filtered.length} tickets to clipboard!`, 'success');
        } catch (err) {
            toastStore.add('Failed to copy: ' + err.message, 'error');
        }
    }
</script>

<div class="analysis-page">
    <header>
        <h1>AI Analysis & Research</h1>
        <p class="subtitle">Sandbox for analyzing database records, finding patterns, and generating LLM prompts.</p>
    </header>

    <div class="grid">
        <div class="card">
            <div class="card-header">
                <h2>Support Knowledge Extractor</h2>
                <span class="badge">Ready</span>
            </div>
            <p class="desc">
                Pulls all support ticket conversations, strips HTML formatting, and builds a massive text block.
                You can copy this directly into Claude or ChatGPT to identify recurring issues or generate troubleshooting guides.
            </p>

            <div class="controls">
                <button class="btn secondary" on:click={fetchDump} disabled={loading}>
                    {loading ? 'Fetching DB...' : '1. Fetch Database Records'}
                </button>
                <span class="record-count">
                    {#if dumpData.length > 0}
                        {dumpData.length} records loaded in memory
                    {/if}
                </span>
            </div>

            <div class="prompt-builder" class:disabled={dumpData.length === 0}>
                <label>Filter Tickets to Include:</label>
                <select bind:value={filterStatus}>
                    <option value="Closed">Closed / Resolved Only</option>
                    <option value="All">All Tickets</option>
                    <option value="Open">Open Only</option>
                </select>

                <label>System Prompt (Instructions for AI):</label>
                <textarea bind:value={systemPrompt} rows="8"></textarea>

                <button class="btn primary" on:click={copyPromptToAI} disabled={dumpData.length === 0}>
                    2. Copy Full Prompt to AI
                </button>
            </div>
        </div>

        <div class="card wip">
            <div class="card-header">
                <h2>Repair Statistics</h2>
                <span class="badge wip-badge">WIP</span>
            </div>
            <p class="desc">
                Future module: Will aggregate data from the <code>orders</code> table (Repairs) to show charts:
                average labor hours, most replaced parts by device model, and warranty vs non-warranty ratios.
            </p>
            <div class="placeholder-box">
                Data Visualization coming soon...
            </div>
        </div>

        <div class="card wip">
            <div class="card-header">
                <h2>Automated Issue Resolution (RAG)</h2>
                <span class="badge wip-badge">WIP</span>
            </div>
            <p class="desc">
                Future module: Instead of manually copying text to ChatGPT, this tool will use our internal Gemini API integration
                (via <code>rig-core</code>) to automatically search past tickets and suggest a solution for a specific problem.
            </p>
            <div class="placeholder-box">
                Internal AI Vector Search coming soon...
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

    @media (max-width: 900px) { .grid { grid-template-columns: 1fr; } }
</style>
