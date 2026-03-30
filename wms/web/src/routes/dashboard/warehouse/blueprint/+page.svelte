<script>
    import { onMount } from 'svelte';
    import { api } from '$lib/api';
    import { toastStore } from '$lib/stores/toastStore';
    import { base } from '$app/paths';

    let racks = [];
    let selectedRack = null;
    let loading = true;

    // Form state
    let formData = {
        name: '',
        columns: 1,
        rows: 1,
        startIndex: '',
        visualWidth: 0,
        visualHeight: 0,
        rotation: 0
    };

    // Drag state
    let isDragging = false;
    let dragTarget = null;
    let dragOffset = { x: 0, y: 0 };
    let canvasEl;

    onMount(async () => {
        await loadRacks();
        loading = false;
    });

    async function loadRacks() {
        try {
            racks = await api.get('/api/warehouse/racks');
        } catch (e) {
            toastStore.add('Failed to load racks', 'error');
        }
    }

    function selectRack(rack) {
        selectedRack = rack;
        formData = {
            name: rack.name || '',
            columns: rack.columns || 1,
            rows: rack.rows || 1,
            startIndex: rack.startIndex || '',
            visualWidth: rack.visualWidth || 0,
            visualHeight: rack.visualHeight || 0,
            rotation: rack.rotation || 0
        };
    }

    function resetForm() {
        selectedRack = null;
        formData = {
            name: '',
            columns: 1,
            rows: 1,
            startIndex: '',
            visualWidth: 0,
            visualHeight: 0,
            rotation: 0
        };
    }

    async function saveRack() {
        if (!formData.name || !formData.columns || !formData.rows) {
            toastStore.add('Please fill Name, Cols, and Rows', 'error');
            return;
        }

        const data = {
            id: selectedRack?.id || null,
            name: formData.name,
            columns: parseInt(formData.columns),
            rows: parseInt(formData.rows),
            startIndex: formData.startIndex === '' ? -1 : parseInt(formData.startIndex),
            visualWidth: parseInt(formData.visualWidth) || 0,
            visualHeight: parseInt(formData.visualHeight) || 0,
            rotation: parseInt(formData.rotation) || 0,
            posX: selectedRack?.posX || 0,
            posY: selectedRack?.posY || 0
        };

        try {
            await api.post('/api/warehouse/racks', data);
            toastStore.add('Rack saved!', 'success');
            if (!selectedRack) resetForm();
            await loadRacks();
        } catch (e) {
            toastStore.add('Failed to save rack', 'error');
        }
    }

    async function deleteRack() {
        if (!selectedRack || !confirm('Delete this rack?')) return;
        try {
            await api.delete(`/api/warehouse/racks/${selectedRack.id}`);
            resetForm();
            await loadRacks();
            toastStore.add('Rack deleted', 'success');
        } catch (e) {
            toastStore.add('Failed to delete rack', 'error');
        }
    }

    // Drag & Drop handlers
    function startDrag(e, rack) {
        isDragging = true;
        dragTarget = rack;
        selectRack(rack);

        const node = e.currentTarget;
        const rect = node.getBoundingClientRect();
        dragOffset.x = e.clientX - rect.left;
        dragOffset.y = e.clientY - rect.top;

        window.addEventListener('mousemove', onDrag);
        window.addEventListener('mouseup', stopDrag);
    }

    function onDrag(e) {
        if (!isDragging || !dragTarget || !canvasEl) return;

        const canvasRect = canvasEl.getBoundingClientRect();
        let x = e.clientX - canvasRect.left - dragOffset.x;
        let y = e.clientY - canvasRect.top - dragOffset.y;

        // Snap to 10px grid
        x = Math.round(x / 10) * 10;
        y = Math.round(y / 10) * 10;

        // Constrain to canvas
        x = Math.max(0, x);
        y = Math.max(0, y);

        // Update the rack position in our local array
        const idx = racks.findIndex(r => r.id === dragTarget.id);
        if (idx !== -1) {
            racks[idx] = { ...racks[idx], posX: x, posY: y };
            racks = racks; // Trigger reactivity
        }
    }

    async function stopDrag() {
        if (isDragging && dragTarget) {
            const rack = racks.find(r => r.id === dragTarget.id);
            if (rack) {
                try {
                    await api.post('/api/warehouse/racks', rack);
                } catch (e) {
                    console.error('Auto-save position failed', e);
                }
            }
        }

        isDragging = false;
        dragTarget = null;
        window.removeEventListener('mousemove', onDrag);
        window.removeEventListener('mouseup', stopDrag);
    }

    // Calculate rack display size
    function getRackSize(rack) {
        let w = rack.visualWidth > 0 ? rack.visualWidth : rack.columns * 50;
        let h = rack.visualHeight > 0 ? rack.visualHeight : rack.rows * 50;
        return { w, h };
    }

    // Calculate text rotation to keep it readable
    function getTextRotation(rackRotation) {
        const rot = rackRotation || 0;
        let desiredAngle = (rot === 90 || rot === 270) ? 90 : 0;
        return desiredAngle - rot;
    }

    function handleCanvasClick(e) {
        if (e.target === canvasEl || e.target.classList.contains('canvas-container')) {
            resetForm();
        }
    }

    function getTotalPlaces(rack) {
        return rack.columns * rack.rows;
    }
</script>

<div class="blueprint-page">
    <header>
        <a href="{base}/dashboard/warehouse" class="back-link">Home</a>
        <h1>Warehouse Blueprint</h1>
        <div class="rack-count">{racks.length} rack{racks.length !== 1 ? 's' : ''}</div>
    </header>

    {#if loading}
        <div class="loading">Loading...</div>
    {:else}
        <div class="main-layout">
            <!-- Sidebar -->
            <div class="sidebar">
                <h2>Rack Management</h2>

                <!-- Rack Form -->
                <div class="rack-form">
                    <div class="form-group">
                        <label>Name</label>
                        <input type="text" bind:value={formData.name} placeholder="e.g. Regal A">
                    </div>

                    <div class="form-row">
                        <div class="form-group">
                            <label>Cols</label>
                            <input type="number" bind:value={formData.columns} min="1" placeholder="5">
                        </div>
                        <div class="form-group">
                            <label>Rows</label>
                            <input type="number" bind:value={formData.rows} min="1" placeholder="10">
                        </div>
                    </div>

                    <div class="form-group">
                        <label>Start ID</label>
                        <input type="number" bind:value={formData.startIndex} placeholder="Auto">
                    </div>

                    <div class="form-row">
                        <div class="form-group">
                            <label>Width (px)</label>
                            <input type="number" bind:value={formData.visualWidth} placeholder="Auto">
                        </div>
                        <div class="form-group">
                            <label>Height (px)</label>
                            <input type="number" bind:value={formData.visualHeight} placeholder="Auto">
                        </div>
                    </div>

                    <div class="form-group">
                        <label>Rotation</label>
                        <select bind:value={formData.rotation}>
                            <option value={0}>0 (Horizontal)</option>
                            <option value={90}>90 (Vertical)</option>
                            <option value={180}>180 (Inverted)</option>
                            <option value={270}>270 (Vert. Inverted)</option>
                        </select>
                    </div>

                    <div class="form-row">
                        <button class="btn btn-primary" on:click={saveRack}>
                            {selectedRack ? 'Update' : 'Create'} Rack
                        </button>
                        <button class="btn btn-secondary" on:click={resetForm}>Clear</button>
                    </div>

                    {#if selectedRack}
                        <button class="btn btn-delete" on:click={deleteRack}>Delete Rack</button>
                    {/if}
                </div>

                <!-- Racks List -->
                <div class="racks-list">
                    {#each racks as rack (rack.id)}
                        <!-- svelte-ignore a11y-click-events-have-key-events -->
                        <!-- svelte-ignore a11y-no-static-element-interactions -->
                        <div
                            class="rack-item"
                            class:active={selectedRack?.id === rack.id}
                            on:click={() => selectRack(rack)}
                        >
                            <div class="name">{rack.name || `Rack #${rack.id}`}</div>
                            <div class="meta">{rack.columns}x{rack.rows} = {getTotalPlaces(rack)} places | Start: {rack.startIndex}</div>
                        </div>
                    {/each}
                    {#if racks.length === 0}
                        <div class="no-racks">No racks yet. Create one above.</div>
                    {/if}
                </div>
            </div>

            <!-- Canvas Area -->
            <!-- svelte-ignore a11y-click-events-have-key-events -->
            <!-- svelte-ignore a11y-no-static-element-interactions -->
            <div class="canvas-area" bind:this={canvasEl} on:click={handleCanvasClick}>
                <div class="canvas-container">
                    {#each racks as rack (rack.id)}
                        {@const size = getRackSize(rack)}
                        {@const textRot = getTextRotation(rack.rotation)}
                        <!-- svelte-ignore a11y-no-static-element-interactions -->
                        <div
                            class="rack-node"
                            class:selected={selectedRack?.id === rack.id}
                            style="
                                left: {rack.posX || 0}px;
                                top: {rack.posY || 0}px;
                                width: {size.w}px;
                                height: {size.h}px;
                                transform: rotate({rack.rotation || 0}deg);
                            "
                            on:mousedown={(e) => startDrag(e, rack)}
                            on:click|stopPropagation={() => selectRack(rack)}
                        >
                            <!-- Indicator dot (pin 1) -->
                            <div class="indicator-dot"></div>

                            <!-- Label -->
                            <div class="rack-label" style="transform: rotate({textRot}deg);">
                                <span class="rack-name">{rack.name || `#${rack.id}`}</span>
                                <span class="dims">{rack.columns}x{rack.rows}</span>
                            </div>
                        </div>
                    {/each}
                </div>
            </div>
        </div>
    {/if}
</div>

<style>
    :root {
        --bg-color: #121212;
        --card-bg: #1E1E1E;
        --text-main: #C0C0C0;
        --text-secondary: #888888;
        --accent-blue: #5a7ba9;
        --accent-hover: #4a6b99;
        --success: #0d9488;
        --warning: #b45309;
        --danger: #991b1b;
        --border: #2a2a2a;
        --grid-line: #1a1a1a;
    }

    .blueprint-page {
        display: flex;
        flex-direction: column;
        height: calc(100vh - 4rem);
        color: var(--text-main);
    }

    header {
        display: flex;
        justify-content: space-between;
        align-items: center;
        margin-bottom: 1rem;
        flex-shrink: 0;
    }

    header h1 {
        margin: 0;
        font-size: 1.5rem;
        color: #fff;
    }

    .back-link {
        color: var(--accent-blue);
        text-decoration: none;
        font-size: 0.9rem;
    }

    .rack-count {
        color: var(--text-secondary);
        font-size: 0.85rem;
    }

    .main-layout {
        display: grid;
        grid-template-columns: 320px 1fr;
        gap: 20px;
        flex-grow: 1;
        overflow: hidden;
    }

    /* Sidebar */
    .sidebar {
        background: var(--card-bg);
        border: 1px solid var(--border);
        border-radius: 12px;
        padding: 20px;
        display: flex;
        flex-direction: column;
        gap: 16px;
        overflow-y: auto;
    }

    .sidebar h2 {
        margin: 0;
        font-size: 1.2rem;
        color: #fff;
    }

    /* Form */
    .rack-form {
        display: flex;
        flex-direction: column;
        gap: 12px;
        background: rgba(255, 255, 255, 0.03);
        padding: 15px;
        border-radius: 8px;
        border: 1px solid var(--border);
    }

    .form-group {
        display: flex;
        flex-direction: column;
        gap: 4px;
    }

    .form-group label {
        font-size: 0.8rem;
        color: var(--text-secondary);
    }

    .form-row {
        display: grid;
        grid-template-columns: 1fr 1fr;
        gap: 10px;
    }

    input, select {
        width: 100%;
        padding: 10px;
        background: #111;
        border: 1px solid var(--border);
        border-radius: 6px;
        color: white;
        font-size: 0.9rem;
    }

    input:focus, select:focus {
        outline: none;
        border-color: var(--accent-blue);
    }

    .btn {
        padding: 12px;
        border: none;
        border-radius: 6px;
        cursor: pointer;
        font-weight: 600;
        transition: all 0.2s;
    }

    .btn-primary {
        background: var(--accent-blue);
        color: white;
    }

    .btn-primary:hover {
        background: var(--accent-hover);
    }

    .btn-secondary {
        background: #333;
        color: #ddd;
    }

    .btn-delete {
        background: var(--danger);
        color: white;
        margin-top: 5px;
    }

    /* Racks List */
    .racks-list {
        display: flex;
        flex-direction: column;
        gap: 8px;
    }

    .rack-item {
        background: #252525;
        padding: 10px;
        border-radius: 6px;
        border: 1px solid var(--border);
        cursor: pointer;
        transition: all 0.2s;
    }

    .rack-item:hover {
        border-color: var(--accent-blue);
    }

    .rack-item.active {
        border-color: var(--accent-blue);
        background: rgba(90, 123, 169, 0.1);
    }

    .rack-item .name {
        font-weight: 600;
        font-size: 0.9rem;
        color: #e0e0e0;
    }

    .rack-item .meta {
        font-size: 0.75rem;
        color: var(--text-secondary);
        margin-top: 2px;
    }

    .no-racks {
        color: var(--text-secondary);
        font-style: italic;
        text-align: center;
        padding: 1rem;
    }

    /* Canvas */
    .canvas-area {
        background: var(--bg-color);
        border: 1px solid var(--border);
        border-radius: 12px;
        position: relative;
        overflow: auto;
        background-image:
            linear-gradient(var(--grid-line) 1px, transparent 1px),
            linear-gradient(90deg, var(--grid-line) 1px, transparent 1px);
        background-size: 50px 50px;
        cursor: crosshair;
    }

    .canvas-container {
        position: relative;
        min-width: 100%;
        min-height: 100%;
        width: max-content;
        height: max-content;
    }

    /* Rack Node */
    .rack-node {
        position: absolute;
        background: var(--accent-blue);
        border: 2px solid rgba(255, 255, 255, 0.2);
        border-radius: 4px;
        cursor: move;
        display: flex;
        flex-direction: column;
        align-items: center;
        justify-content: center;
        color: white;
        font-size: 0.8rem;
        font-weight: bold;
        text-align: center;
        padding: 5px;
        box-shadow: 0 4px 10px rgba(0, 0, 0, 0.5);
        transition: box-shadow 0.2s;
        user-select: none;
        min-width: 60px;
        min-height: 40px;
        transform-origin: center;
    }

    .rack-node:hover {
        box-shadow: 0 0 15px var(--accent-blue);
        z-index: 100;
    }

    .rack-node.selected {
        border-color: white;
        box-shadow: 0 0 20px rgba(255, 255, 255, 0.4);
        z-index: 101;
    }

    /* Indicator dot */
    .indicator-dot {
        position: absolute;
        width: 8px;
        height: 8px;
        border-radius: 50%;
        background-color: white;
        bottom: 4px;
        left: 4px;
        box-shadow: 0 0 3px rgba(0,0,0,0.5);
        z-index: 10;
    }

    /* Rack Label */
    .rack-label {
        font-weight: bold;
        font-size: 0.85rem;
        white-space: nowrap;
        text-shadow: 0 1px 2px rgba(0,0,0,0.8);
        display: flex;
        gap: 6px;
        align-items: center;
    }

    .rack-label .rack-name {
        color: #a3e635;
        font-weight: 700;
    }

    .rack-label .dims {
        color: #fbbf24;
        font-weight: 600;
    }

    .loading {
        text-align: center;
        padding: 3rem;
        color: var(--text-secondary);
    }
</style>
