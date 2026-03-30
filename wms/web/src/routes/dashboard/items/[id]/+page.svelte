<script>
    import { page } from '$app/stores';
    import { onMount } from 'svelte';
    import { api } from '$lib/api';
    import { goto } from '$app/navigation';
    import { toastStore } from '$lib/stores/toastStore';
    import { base } from '$app/paths';

    let item = null;
    let attachments = [];
    let loading = true;
    let isEditing = false;
    let error = null;

    // Clone for editing
    let editForm = {};

    // AI Analysis state
    let analyzingFileId = null;
    let analysisResult = null; // { fileId, data }


    onMount(async () => {
        await loadItem();
    });

    async function loadItem() {
        try {
            item = await api.get(`/api/items/${$page.params.id}`);
            editForm = { ...item };
            // Load attachments after item is available
            await loadAttachments();
        } catch (e) {
            error = e.message;
        } finally {
            loading = false;
        }
    }

    async function loadAttachments() {
        if (!item) return;
        const barcode = item.barcode;
        if (!barcode) return;
        try {
            attachments = await api.get(`/api/attachments/product/${encodeURIComponent(barcode)}`);
        } catch (e) {
            console.warn("Failed to load attachments", e);
        }
    }

    function toggleEdit() {
        if (isEditing) {
            // Cancelled
            editForm = { ...item };
        }
        isEditing = !isEditing;
    }

    async function saveItem() {
        try {
            const updated = await api.put(`/api/items/${item.id}`, editForm);
            item = updated;
            editForm = { ...item };
            isEditing = false;
            toastStore.add('Item updated successfully', 'success');
        } catch (e) {
            toastStore.add(e.message, 'error');
        }
    }

    function goBack() {
        goto(`${base}/dashboard/items`);
    }

    function fileUrl(fileId) {
        return `${base}/api/files/${fileId}`;
    }

    async function analyzeImage(fileId) {
        analyzingFileId = fileId;
        analysisResult = null;
        try {
            const res = await api.post('/api/ai/analyze-image', { file_id: fileId });
            if (res.success) {
                analysisResult = { fileId, data: res.analysis };
                toastStore.add('Analysis complete', 'success');
            }
        } catch (e) {
            if (e.message?.includes('503') || e.message?.includes('unavailable') || e.message?.includes('Service')) {
                toastStore.add('AI Analysis unavailable (Check API Key or Config)', 'error', 5000);
            } else {
                toastStore.add(e.message || 'Analysis failed', 'error');
            }
        } finally {
            analyzingFileId = null;
        }
    }
</script>

<div class="detail-page">
    <div class="header">
        <button class="back-btn" on:click={goBack}>&larr; Back</button>
        <div class="title-row">
            {#if item}
                <h1>{isEditing ? 'Editing: ' : ''}{item.name}</h1>
                <div class="actions">
                    {#if isEditing}
                        <button class="btn secondary" on:click={toggleEdit}>Cancel</button>
                        <button class="btn primary" on:click={saveItem}>Save</button>
                    {:else}
                        <button class="btn primary" on:click={toggleEdit}>Edit Item</button>
                    {/if}
                </div>
            {:else}
                <h1>Item Details</h1>
            {/if}
        </div>
    </div>

    {#if loading}
        <div class="loading">Loading details...</div>
    {:else if error}
        <div class="error">Error: {error}</div>
    {:else if item}
        <div class="detail-grid">
            <!-- Main Info -->
            <div class="section main-info">
                <h3>Basic Information</h3>
                <div class="field">
                    <label>SKU</label>
                    {#if isEditing}
                        <input type="text" bind:value={editForm.default_code} class="code-input" />
                    {:else}
                        <div class="value code">{item.default_code || '-'}</div>
                    {/if}
                </div>
                <div class="field">
                    <label>Name</label>
                    {#if isEditing}
                        <input type="text" bind:value={editForm.name} />
                    {:else}
                        <div class="value">{item.name}</div>
                    {/if}
                </div>
                <div class="field">
                    <label>Barcode</label>
                    {#if isEditing}
                        <input type="text" bind:value={editForm.barcode} class="code-input" />
                    {:else}
                        <div class="value code">{item.barcode || '-'}</div>
                    {/if}
                </div>
                <div class="field">
                    <label>Type</label>
                    <div class="value">{item.type || '-'}</div>
                </div>
            </div>

            <!-- Stats -->
            <div class="section stats">
                <h3>Inventory Status</h3>
                <div class="stat-box">
                    <span class="label">List Price</span>
                    <span class="val">${item.list_price?.toFixed(2) || '0.00'}</span>
                </div>
                <div class="stat-box">
                    <span class="label">Cost Price</span>
                    <span class="val">${item.standard_price?.toFixed(2) || '0.00'}</span>
                </div>
                <div class="stat-box">
                    <span class="label">Weight</span>
                    <span class="val">{item.weight || 0} kg</span>
                </div>
                <div class="stat-box">
                    <span class="label">Volume</span>
                    <span class="val">{item.volume || 0} m&sup3;</span>
                </div>
                <div class="stat-box">
                    <span class="label">Status</span>
                    <span class="val {item.active ? 'active' : 'inactive'}">{item.active ? 'Active' : 'Inactive'}</span>
                </div>
            </div>

            <!-- Photos / Gallery -->
            {#if attachments.length > 0}
                <div class="section gallery-section">
                    <h3>Visual Evidence ({attachments.length})</h3>
                    <div class="gallery-grid">
                        {#each attachments as file}
                            <div class="photo-card" class:main={file.is_main}>
                                <img src={fileUrl(file.file_id)} alt="Item photo" loading="lazy" />
                                {#if file.is_main}
                                    <span class="badge">MAIN</span>
                                {/if}
                                <button
                                    class="ai-btn"
                                    on:click|stopPropagation={() => analyzeImage(file.file_id)}
                                    disabled={analyzingFileId === file.file_id}
                                    title="AI Analyze"
                                >
                                    {#if analyzingFileId === file.file_id}
                                        <span class="spinner">&#x21bb;</span>
                                    {:else}
                                        &#x2728;
                                    {/if}
                                </button>
                            </div>
                        {/each}
                    </div>

                    {#if analysisResult}
                        <div class="analysis-overlay">
                            <div class="analysis-header">
                                <h4>AI Analysis</h4>
                                <button class="close-btn" on:click={() => analysisResult = null}>&times;</button>
                            </div>
                            <div class="analysis-content">
                                {#if analysisResult.data.condition}
                                    <div class="tag-row">
                                        <span class="tag condition {analysisResult.data.condition}">{analysisResult.data.condition}</span>
                                        {#if analysisResult.data.labels_visible !== undefined}
                                            <span class="tag {analysisResult.data.labels_visible ? 'good' : 'damaged'}">
                                                Labels: {analysisResult.data.labels_visible ? 'Visible' : 'Missing'}
                                            </span>
                                        {/if}
                                    </div>
                                {/if}
                                {#if analysisResult.data.description}
                                    <p><strong>Description:</strong> {analysisResult.data.description}</p>
                                {/if}
                                {#if analysisResult.data.ocr_text}
                                    <p><strong>OCR:</strong> <code>{analysisResult.data.ocr_text}</code></p>
                                {/if}
                                {#if analysisResult.data.tags}
                                    <div class="tags">
                                        {#each analysisResult.data.tags as tag}
                                            <span class="pill">{tag}</span>
                                        {/each}
                                    </div>
                                {/if}
                                {#if analysisResult.data.raw_analysis}
                                    <p class="raw"><code>{analysisResult.data.raw_analysis}</code></p>
                                {/if}
                            </div>
                        </div>
                    {/if}
                </div>
            {/if}
        </div>
    {/if}
</div>

<style>
    .detail-page { max-width: 1000px; margin: 0 auto; }

    .header { margin-bottom: 2rem; }
    .back-btn { background: none; border: none; color: #888; cursor: pointer; font-size: 1rem; padding: 0; margin-bottom: 1rem; }
    .back-btn:hover { color: #fff; }

    .title-row { display: flex; justify-content: space-between; align-items: center; }
    h1 { color: #fff; font-size: 2rem; margin: 0; }

    .actions { display: flex; gap: 10px; }

    .btn { padding: 0.6rem 1.2rem; border-radius: 4px; border: none; font-weight: 600; cursor: pointer; font-size: 0.9rem; }
    .btn.primary { background: #4a69bd; color: white; }
    .btn.secondary { background: #444; color: #ccc; }
    .btn:hover { opacity: 0.9; }

    .detail-grid { display: grid; grid-template-columns: 2fr 1fr; gap: 2rem; }

    .section { background: #1e1e1e; border: 1px solid #333; border-radius: 8px; padding: 1.5rem; }
    .section h3 { margin-top: 0; color: #6bc5f0; font-size: 1.1rem; border-bottom: 1px solid #333; padding-bottom: 10px; margin-bottom: 15px; }

    .main-info { display: grid; grid-template-columns: 1fr 1fr; gap: 1.5rem; }
    .field label { display: block; color: #666; font-size: 0.8rem; text-transform: uppercase; margin-bottom: 0.4rem; }
    .field .value { color: #e0e0e0; font-size: 1.1rem; }
    .field .code { font-family: monospace; background: #2a2a2a; padding: 2px 6px; border-radius: 4px; display: inline-block; }

    /* Gallery */
    .gallery-section { grid-column: 1 / -1; }
    .gallery-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(150px, 1fr)); gap: 1rem; }
    .photo-card {
        position: relative;
        aspect-ratio: 1;
        background: #000;
        border-radius: 8px;
        overflow: hidden;
        border: 1px solid #444;
    }
    .photo-card img { width: 100%; height: 100%; object-fit: cover; transition: transform 0.3s; }
    .photo-card:hover img { transform: scale(1.05); }
    .photo-card.main { border-color: #4a69bd; box-shadow: 0 0 10px rgba(74, 105, 189, 0.3); }
    .badge {
        position: absolute; bottom: 5px; right: 5px;
        background: #4a69bd; color: white;
        font-size: 0.7rem; padding: 2px 6px; border-radius: 4px;
        font-weight: bold;
    }

    /* Inputs */
    input, textarea, select {
        width: 100%; background: #121212; border: 1px solid #444; color: white;
        padding: 8px; border-radius: 4px; font-size: 1rem; box-sizing: border-box;
    }
    input:focus, textarea:focus { border-color: #4a69bd; outline: none; }
    .code-input { font-family: monospace; }

    .stats { display: flex; flex-direction: column; gap: 1rem; }
    .stat-box { display: flex; justify-content: space-between; align-items: center; border-bottom: 1px solid #333; padding-bottom: 0.5rem; }
    .stat-box:last-child { border-bottom: none; padding-bottom: 0; }
    .stat-box .label { color: #888; }
    .stat-box .val { color: #fff; font-weight: 700; font-size: 1.2rem; }
    .stat-box .active { color: #28a745; }
    .stat-box .inactive { color: #555; }

    /* AI Analyze Button */
    .ai-btn {
        position: absolute; top: 6px; right: 6px;
        width: 32px; height: 32px;
        background: rgba(0,0,0,0.6); border: 1px solid #555; border-radius: 6px;
        color: #fff; font-size: 1rem; cursor: pointer;
        display: flex; align-items: center; justify-content: center;
        opacity: 0; transition: opacity 0.2s;
    }
    .photo-card:hover .ai-btn { opacity: 1; }
    .ai-btn:hover { background: rgba(74,105,189,0.8); border-color: #4a69bd; }
    .ai-btn:disabled { cursor: wait; opacity: 1; }
    .spinner { display: inline-block; animation: spin 1s linear infinite; }
    @keyframes spin { to { transform: rotate(360deg); } }

    /* Analysis Result Overlay */
    .analysis-overlay {
        margin-top: 1rem; background: #161616; border: 1px solid #4a69bd;
        border-radius: 8px; padding: 1rem; animation: fadeIn 0.2s ease;
    }
    @keyframes fadeIn { from { opacity: 0; transform: translateY(-8px); } to { opacity: 1; transform: translateY(0); } }
    .analysis-header { display: flex; justify-content: space-between; align-items: center; margin-bottom: 0.75rem; }
    .analysis-header h4 { margin: 0; color: #6bc5f0; font-size: 1rem; }
    .close-btn { background: none; border: none; color: #888; font-size: 1.4rem; cursor: pointer; padding: 0; line-height: 1; }
    .close-btn:hover { color: #fff; }

    .analysis-content p { color: #ccc; margin: 0.5rem 0; font-size: 0.9rem; }
    .analysis-content code { background: #2a2a2a; padding: 2px 6px; border-radius: 3px; font-size: 0.85rem; color: #e0e0e0; }
    .analysis-content .raw code { display: block; white-space: pre-wrap; padding: 0.5rem; margin-top: 0.25rem; }

    .tag-row { display: flex; gap: 8px; flex-wrap: wrap; margin-bottom: 0.5rem; }
    .tag { padding: 3px 10px; border-radius: 4px; font-size: 0.8rem; font-weight: 600; text-transform: capitalize; }
    .tag.condition.good { background: #1b4332; color: #52b788; }
    .tag.condition.damaged { background: #4a1010; color: #e07070; }
    .tag.condition.unknown, .tag.condition.open { background: #3a3000; color: #e0c040; }
    .tag.good { background: #1b4332; color: #52b788; }
    .tag.damaged { background: #4a1010; color: #e07070; }

    .tags { display: flex; flex-wrap: wrap; gap: 6px; margin-top: 0.5rem; }
    .pill { background: #2a2a2a; color: #aaa; padding: 3px 10px; border-radius: 12px; font-size: 0.8rem; border: 1px solid #444; }

    @media (max-width: 800px) {
        .detail-grid { grid-template-columns: 1fr; }
        .main-info { grid-template-columns: 1fr; }
    }
</style>
