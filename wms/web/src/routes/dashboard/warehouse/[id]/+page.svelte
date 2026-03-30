<script>
    import { page } from '$app/stores';
    import { onMount } from 'svelte';
    import { api } from '$lib/api';
    import { goto } from '$app/navigation';
    import { base } from '$app/paths';
    import { toastStore } from '$lib/stores/toastStore';

    let whId = $page.params.id;
    let warehouse = null;
    let loading = true;
    let error = null;
    let racks = [];

    // Edit Mode State
    let isEditing = false;
    let selectedRack = null;
    let isDragging = false;
    let dragOffset = { x: 0, y: 0 };
    let dragTargetId = null;

    // Rack Form
    let rackForm = {
        name: '',
        columns: 1,
        rows: 1,
        rotation: 0
    };

    // Canvas settings
    const GRID_SIZE = 50; // px

    onMount(async () => {
        await loadWarehouse();
    });

    async function loadWarehouse() {
        try {
            loading = true;
            warehouse = await api.get(`/api/warehouse/${whId}`);
            racks = warehouse.racks || [];
        } catch (e) {
            error = e.message;
            toastStore.add('Error loading warehouse', 'error');
        } finally {
            loading = false;
        }
    }

    function goBack() {
        goto(`${base}/dashboard/warehouse`);
    }

    function toggleEdit() {
        isEditing = !isEditing;
        selectedRack = null;
    }

    function getRackStyle(rack) {
        const width = (rack.visualWidth > 0 ? rack.visualWidth : rack.columns * GRID_SIZE);
        const height = (rack.visualHeight > 0 ? rack.visualHeight : rack.rows * GRID_SIZE);

        return `
            left: ${rack.posX || 0}px;
            top: ${rack.posY || 0}px;
            width: ${width}px;
            height: ${height}px;
            transform: rotate(${rack.rotation || 0}deg);
        `;
    }

    // --- Drag & Drop Logic ---

    function startDrag(event, rack) {
        if (!isEditing) return;
        event.stopPropagation(); // Prevent canvas click

        isDragging = true;
        dragTargetId = rack.id;
        selectRack(rack);

        // Calculate offset
        // We need to account for the canvas scroll if any, but event.clientX is viewport relative
        // rack.posX is relative to canvas.
        const rackEl = event.currentTarget;
        const rect = rackEl.getBoundingClientRect();
        dragOffset = {
            x: event.clientX - rect.left,
            y: event.clientY - rect.top
        };

        window.addEventListener('mousemove', onDrag);
        window.addEventListener('mouseup', stopDrag);
    }

    function onDrag(event) {
        if (!isDragging || !dragTargetId) return;

        const canvasEl = document.querySelector('.canvas');
        const canvasRect = canvasEl.getBoundingClientRect();

        // Calculate new position relative to canvas
        let x = event.clientX - canvasRect.left - dragOffset.x;
        let y = event.clientY - canvasRect.top - dragOffset.y;

        // Snap to grid (10px)
        x = Math.round(x / 10) * 10;
        y = Math.round(y / 10) * 10;

        // Constraint
        x = Math.max(0, x);
        y = Math.max(0, y);

        // Update local state immediately for responsiveness
        const idx = racks.findIndex(r => r.id === dragTargetId);
        if (idx !== -1) {
            racks[idx].posX = x;
            racks[idx].posY = y;
            racks = [...racks]; // Trigger reactivity
        }
    }

    async function stopDrag() {
        if (!isDragging) return;

        // Save position
        const rack = racks.find(r => r.id === dragTargetId);
        if (rack) {
            await saveRackPosition(rack);
        }

        isDragging = false;
        dragTargetId = null;
        window.removeEventListener('mousemove', onDrag);
        window.removeEventListener('mouseup', stopDrag);
    }

    async function saveRackPosition(rack) {
        try {
            await api.put(`/api/warehouse/racks/${rack.id}`, {
                posX: rack.posX,
                posY: rack.posY
            });
        } catch (e) {
            toastStore.add('Failed to save position', 'error');
        }
    }

    // --- Form Logic ---

    function selectRack(rack) {
        selectedRack = rack;
        rackForm = {
            name: rack.name,
            columns: rack.columns,
            rows: rack.rows,
            rotation: rack.rotation || 0
        };
    }

    function deselectRack() {
        selectedRack = null;
    }

    async function saveRackDetails() {
        if (!selectedRack) return;

        try {
            const updated = await api.put(`/api/warehouse/racks/${selectedRack.id}`, rackForm);

            // Update local list
            const idx = racks.findIndex(r => r.id === selectedRack.id);
            if (idx !== -1) {
                racks[idx] = { ...racks[idx], ...updated };
                racks = [...racks];
            }
            toastStore.add('Rack updated', 'success');
        } catch (e) {
            toastStore.add(e.message, 'error');
        }
    }

    async function createRack() {
        const name = prompt("New Rack Name:");
        if (!name) return;

        try {
            const newRack = await api.post('/api/warehouse/racks', {
                warehouseId: parseInt(whId),
                name: name,
                columns: 1,
                rows: 1,
                posX: 50,
                posY: 50,
                startIndex: 0 // Backend handles auto-increment logic ideally, or 0
            });
            racks = [...racks, newRack];
            selectRack(newRack);
            toastStore.add('Rack created', 'success');
        } catch (e) {
            toastStore.add(e.message, 'error');
        }
    }

    async function deleteRack() {
        if (!selectedRack || !confirm('Delete this rack?')) return;
        try {
            await api.delete(`/api/warehouse/racks/${selectedRack.id}`);
            racks = racks.filter(r => r.id !== selectedRack.id);
            selectedRack = null;
            toastStore.add('Rack deleted', 'success');
        } catch (e) {
            toastStore.add(e.message, 'error');
        }
    }

    function onCanvasClick(e) {
        if (e.target.classList.contains('canvas')) {
            deselectRack();
        }
    }
</script>

<div class="blueprint-page">
    <div class="header">
        <button class="back-btn" on:click={goBack}>← Back</button>
        <div class="title-row">
            {#if warehouse}
                <h1>{warehouse.name} <span class="blueprint-label">Blueprint</span></h1>
                <div class="actions">
                    <button class="btn {isEditing ? 'active' : ''}" on:click={toggleEdit}>
                        {isEditing ? 'Done Editing' : 'Edit Layout'}
                    </button>
                </div>
            {:else}
                <h1>Warehouse Blueprint</h1>
            {/if}
        </div>
    </div>

    {#if loading}
        <div class="loading">Loading blueprint...</div>
    {:else if error}
        <div class="error">{error}</div>
    {:else}
        <div class="layout-container">
            <!-- Sidebar for editing -->
            {#if isEditing}
                <div class="editor-sidebar">
                    <h3>Rack Properties</h3>
                    <div class="sidebar-actions">
                        <button class="btn primary full-width" on:click={createRack}>+ Add Rack</button>
                    </div>

                    {#if selectedRack}
                        <div class="edit-form">
                            <div class="form-group">
                                <label>Name</label>
                                <input type="text" bind:value={rackForm.name} />
                            </div>
                            <div class="form-group">
                                <label>Columns</label>
                                <input type="number" bind:value={rackForm.columns} />
                            </div>
                            <div class="form-group">
                                <label>Rows</label>
                                <input type="number" bind:value={rackForm.rows} />
                            </div>
                            <div class="form-group">
                                <label>Rotation (°)</label>
                                <select bind:value={rackForm.rotation}>
                                    <option value={0}>0°</option>
                                    <option value={90}>90°</option>
                                    <option value={180}>180°</option>
                                    <option value={270}>270°</option>
                                </select>
                            </div>
                            <div class="form-actions">
                                <button class="btn secondary" on:click={deleteRack}>Delete</button>
                                <button class="btn primary" on:click={saveRackDetails}>Update</button>
                            </div>
                        </div>
                    {:else}
                        <div class="hint">Select a rack to edit properties.</div>
                    {/if}
                </div>
            {/if}

            <!-- Canvas Viewport -->
            <!-- svelte-ignore a11y-click-events-have-key-events -->
            <!-- svelte-ignore a11y-no-static-element-interactions -->
            <div class="viewport" on:click={onCanvasClick}>
                <div class="canvas">
                    {#each racks as rack}
                        <!-- svelte-ignore a11y-click-events-have-key-events -->
                        <!-- svelte-ignore a11y-no-static-element-interactions -->
                        <div
                            class="rack {selectedRack?.id === rack.id ? 'selected' : ''}"
                            style={getRackStyle(rack)}
                            title="{rack.name}"
                            on:mousedown={(e) => startDrag(e, rack)}
                        >
                            <div class="rack-label" style="transform: rotate(-{rack.rotation || 0}deg);">
                                <span class="rack-name">{rack.name}</span>
                                {#if !isEditing}
                                    <span class="rack-info">{rack.columns}×{rack.rows}</span>
                                {/if}
                            </div>
                        </div>
                    {/each}

                    {#if racks.length === 0}
                        <div class="empty-canvas">No racks. Click 'Edit Layout' to add one.</div>
                    {/if}
                </div>
            </div>
        </div>

        <div class="controls">
            <div class="legend">
                <div class="legend-item"><span class="box rack-box"></span> Rack</div>
                <div class="legend-item"><span class="box selected-box"></span> Selected</div>
            </div>
            <div class="info">
                {racks.length} racks | {isEditing ? 'Edit Mode Active' : 'View Mode'}
            </div>
        </div>
    {/if}
</div>

<style>
    .blueprint-page {
        height: calc(100vh - 4rem);
        display: flex;
        flex-direction: column;
    }

    .header { margin-bottom: 1rem; flex-shrink: 0; }
    .back-btn { background: none; border: none; color: #888; cursor: pointer; font-size: 1rem; padding: 0; margin-bottom: 0.5rem; }
    .back-btn:hover { color: #fff; }

    .title-row { display: flex; justify-content: space-between; align-items: center; }
    h1 { color: #fff; font-size: 1.8rem; margin: 0; display: flex; align-items: center; gap: 10px; }
    .blueprint-label { font-size: 0.8rem; background: #333; padding: 2px 8px; border-radius: 4px; color: #aaa; text-transform: uppercase; letter-spacing: 1px; font-weight: normal; }

    .btn { padding: 0.6rem 1.2rem; border-radius: 4px; border: 1px solid #444; background: #333; color: white; cursor: pointer; }
    .btn:hover { background: #444; }
    .btn.active { background: #4a69bd; border-color: #4a69bd; }
    .btn.primary { background: #28a745; border-color: #28a745; }
    .btn.secondary { background: #d32f2f; border-color: #d32f2f; }
    .btn.full-width { width: 100%; }

    .layout-container {
        display: flex;
        flex: 1;
        overflow: hidden;
        border: 1px solid #333;
        border-radius: 8px;
        background: #1a1a1a;
    }

    .editor-sidebar {
        width: 250px;
        background: #1e1e1e;
        border-right: 1px solid #333;
        padding: 1rem;
        display: flex;
        flex-direction: column;
        gap: 1rem;
        z-index: 10;
        box-shadow: 2px 0 10px rgba(0,0,0,0.3);
    }

    .editor-sidebar h3 { margin: 0; color: #ccc; font-size: 1rem; border-bottom: 1px solid #333; padding-bottom: 0.5rem; }
    .hint { color: #666; font-size: 0.9rem; font-style: italic; margin-top: 1rem; }

    .form-group { margin-bottom: 10px; }
    .form-group label { display: block; font-size: 0.8rem; color: #888; margin-bottom: 4px; }
    .form-group input, .form-group select { width: 100%; background: #111; border: 1px solid #444; color: #fff; padding: 6px; border-radius: 4px; box-sizing: border-box; }

    .form-actions { display: flex; gap: 10px; margin-top: 20px; }

    .viewport {
        flex: 1;
        overflow: auto;
        position: relative;
        background-image: linear-gradient(#222 1px, transparent 1px), linear-gradient(90deg, #222 1px, transparent 1px);
        background-size: 50px 50px;
    }

    .canvas {
        width: 3000px;
        height: 3000px;
        position: relative;
    }

    .rack {
        position: absolute;
        background-color: rgba(74, 105, 189, 0.2);
        border: 2px solid #4a69bd;
        border-radius: 4px;
        display: flex;
        align-items: center;
        justify-content: center;
        cursor: pointer;
        transition: background-color 0.2s, box-shadow 0.2s;
    }

    .rack:hover { background-color: rgba(74, 105, 189, 0.4); z-index: 10; }
    .rack.selected { border-color: #fff; box-shadow: 0 0 10px rgba(255,255,255,0.3); z-index: 100; }

    .rack-label { text-align: center; pointer-events: none; }
    .rack-name { display: block; color: #fff; font-weight: 700; font-size: 0.9rem; text-shadow: 0 1px 2px rgba(0,0,0,0.8); }
    .rack-info { display: block; color: #fbbf24; font-size: 0.75rem; font-weight: 600; }

    .empty-canvas { position: absolute; top: 50px; left: 50px; color: #666; font-style: italic; }

    .controls {
        margin-top: 1rem;
        padding: 1rem;
        background: #1e1e1e;
        border: 1px solid #333;
        border-radius: 8px;
        display: flex;
        justify-content: space-between;
        align-items: center;
        flex-shrink: 0;
    }

    .legend { display: flex; gap: 1.5rem; }
    .legend-item { display: flex; align-items: center; gap: 0.5rem; color: #aaa; font-size: 0.9rem; }
    .box { width: 16px; height: 16px; border-radius: 3px; display: inline-block; }
    .rack-box { background: rgba(74, 105, 189, 0.2); border: 2px solid #4a69bd; }
    .selected-box { background: rgba(255, 255, 255, 0.1); border: 2px solid #fff; }
    .info { color: #666; font-size: 0.9rem; }
</style>
