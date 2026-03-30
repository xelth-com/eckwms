<script>
    import { onMount } from 'svelte';
    import { toastStore } from '$lib/stores/toastStore';

    // Config state
    let selectedType = null;
    let isTightMode = true; // Default to true (Tight/Overlap mode ON)

    // Layout config
    let cols = 3;
    let rows = 7;
    let pages = 1;
    let count = 21;
    let startNumber = 1;

    // Margins (mm)
    let marginTop = 4;
    let marginBottom = 4;
    let marginLeft = 4;
    let marginRight = 4;
    let gapX = 8;
    let gapY = 6;

    // Content styling
    let serialDigits = 6;
    let selectedElement = 'qr1';

    // Style config per element - will be loaded from defaults
    let styleCfg = {
        qr1: { scale: 1, x: 0, y: 0 },
        qr2: { scale: 0.3, x: 62, y: 22 },
        qr3: { scale: 0.3, x: 82, y: 22 },
        checksum: { scale: 0.55, x: 61, y: 60 },
        serial: { scale: 0.15, x: 62, y: 4 }
    };

    // Defaults per type
    const defaults = {
        'i': { // Items
            '3x7': {
                qr1: { scale: 1, x: 0, y: 0 },
                qr2: { scale: 0.3, x: 62, y: 22 },
                qr3: { scale: 0.3, x: 82, y: 22 },
                checksum: { scale: 0.55, x: 61, y: 60 },
                serial: { scale: 0.15, x: 62, y: 4 },
                serialDigits: 6
            },
            '2x8': {
                qr1: { scale: 0.8, x: 5, y: 10 },
                qr2: { scale: 0.3, x: 75, y: 50 },
                qr3: { scale: 0.3, x: 75, y: 10 },
                checksum: { scale: 0.5, x: 40, y: 35 },
                serial: { scale: 0.2, x: 40, y: 10 },
                serialDigits: 6
            }
        },
        'b': { // Boxes
            '3x7': {
                qr1: { scale: 1, x: 0, y: 0 },
                qr2: { scale: 0.3, x: 61, y: 0 },
                qr3: { scale: 0.3, x: 82, y: 0 },
                checksum: { scale: 0.55, x: 61, y: 39 },
                serial: { scale: 0.15, x: 61, y: 87 },
                serialDigits: 6
            },
            '2x8': {
                qr1: { scale: 0.8, x: 5, y: 10 },
                qr2: { scale: 0.3, x: 75, y: 50 },
                qr3: { scale: 0.3, x: 75, y: 10 },
                checksum: { scale: 0.5, x: 40, y: 35 },
                serial: { scale: 0.2, x: 40, y: 10 },
                serialDigits: 6
            }
        },
        'p': { // Places
            '3x7': {
                qr1: { scale: 0.8, x: 0, y: 19 },
                qr2: { scale: 0.4, x: 50, y: 0 },
                qr3: { scale: 0.4, x: 77, y: 0 },
                checksum: { scale: 0.5, x: 50, y: 51 },
                serial: { scale: 0.15, x: 0, y: 0 },
                serialDigits: 8
            },
            '2x8': {
                qr1: { scale: 0.8, x: 5, y: 10 },
                qr2: { scale: 0.3, x: 75, y: 50 },
                qr3: { scale: 0.3, x: 75, y: 10 },
                checksum: { scale: 0.5, x: 40, y: 35 },
                serial: { scale: 0.2, x: 40, y: 10 },
                serialDigits: 6
            }
        },
        'l': { // L-Markers (Labels)
            '3x7': {
                qr1: { scale: 1, x: 0, y: 0 },
                qr2: { scale: 0.3, x: 62, y: 50 },
                qr3: { scale: 0.3, x: 82, y: 50 },
                checksum: { scale: 0.6, x: 60, y: 0 },
                serial: { scale: 0.15, x: 62, y: 87 },
                serialDigits: 6
            },
            '2x8': {
                qr1: { scale: 0.8, x: 5, y: 10 },
                qr2: { scale: 0.3, x: 75, y: 50 },
                qr3: { scale: 0.3, x: 75, y: 10 },
                checksum: { scale: 0.5, x: 40, y: 35 },
                serial: { scale: 0.2, x: 40, y: 10 },
                serialDigits: 6
            }
        }
    };

    // Warehouse config for Places
    let warehouseConfig = { regals: [] };
    let registeredRacks = [];
    let selectedRack = '';
    let showPlanner = false;

    // Rack editor vars
    let rackForm = { name: '', columns: 1, rows: 1, rotation: 0 };

    let loading = false;
    let previewEl;

    // Computed
    $: layoutKey = `${cols}x${rows}`;
    $: count = pages * cols * rows;

    // Label dimensions calculation (For display text)
    $: labelDims = calculateLabelDims(marginTop, marginBottom, marginLeft, marginRight, gapX, gapY, cols, rows);

    function calculateLabelDims(mt, mb, ml, mr, gx, gy, c, r) {
        const pageWidth = 210;
        const pageHeight = 297;

        const workingWidth = pageWidth - ml - mr;
        const workingHeight = pageHeight - mt - mb;

        // In tight mode (overlap), gaps don't consume space from the label size in the same way,
        // but for visual representation we treat them as spacing.
        const totalGapWidth = (c - 1) * gx;
        const totalGapHeight = (r - 1) * gy;

        const w = Math.max(0, (workingWidth - totalGapWidth) / c);
        const h = Math.max(0, (workingHeight - totalGapHeight) / r);

        return { w: w.toFixed(1), h: h.toFixed(1), aspect: h === 0 ? 1 : w / h };
    }

    function selectType(type) {
        if (selectedType) saveConfig(selectedType, cols, rows);
        selectedType = type;
        loadConfig(type, cols, rows);
        if (type === 'p') loadRacks();
    }

    function saveConfig(type, c, r) {
        const key = `${c}x${r}`;
        const saved = JSON.parse(localStorage.getItem('eck_print_layouts') || '{}');
        if (!saved[type]) saved[type] = {};
        saved[type][key] = { ...JSON.parse(JSON.stringify(styleCfg)), serialDigits };
        localStorage.setItem('eck_print_layouts', JSON.stringify(saved));
    }

    function loadConfig(type, c, r) {
        const key = `${c}x${r}`;
        const saved = JSON.parse(localStorage.getItem('eck_print_layouts') || '{}');
        if (saved[type]?.[key]) {
            styleCfg = JSON.parse(JSON.stringify(saved[type][key]));
            serialDigits = saved[type][key].serialDigits || 6;
            return;
        }
        if (defaults[type]?.[key]) {
            styleCfg = JSON.parse(JSON.stringify(defaults[type][key]));
            serialDigits = defaults[type][key].serialDigits || 6;
        }
    }

    function resetToDefault() {
        const type = selectedType || 'i';
        if (defaults[type]?.[layoutKey]) {
            const defaultCfg = JSON.parse(JSON.stringify(defaults[type][layoutKey]));
            styleCfg = defaultCfg;
            serialDigits = defaultCfg.serialDigits || 6;
        }
    }

    function onLayoutChange() {
        if (selectedType) saveConfig(selectedType, cols, rows);
        loadConfig(selectedType || 'i', cols, rows);
    }

    // --- API Calls ---
    async function loadRacks() {
        try {
            const token = localStorage.getItem('auth_token');
            const res = await fetch('/api/warehouse', {
                headers: { 'Authorization': `Bearer ${token}` }
            });
            if (res.ok) {
                const warehouses = await res.json();
                registeredRacks = [];
                warehouses.forEach(wh => {
                    if (wh.racks) {
                        wh.racks.forEach((rack, idx) => {
                            registeredRacks.push({
                                ...rack,
                                warehouse_name: wh.name,
                                sort_order: idx + 1
                            });
                        });
                    }
                });
                buildWarehouseConfig();
            }
        } catch (e) {
            console.error(e);
        }
    }

    function buildWarehouseConfig() {
        warehouseConfig.regals = registeredRacks.map((rack, idx) => ({
            index: rack.sort_order || (idx + 1),
            columns: parseInt(rack.columns) || 10,
            rows: parseInt(rack.rows) || 5,
            start_index: parseInt(rack.start_index) || 0
        }));
    }

    function onRackSelect() {
        if (!selectedRack) return;
        const rack = registeredRacks.find(r => r.id == selectedRack);
        if (rack) {
            startNumber = rack.start_index || 0;
            count = (rack.columns || 10) * (rack.rows || 5);
        }
    }

    async function generatePDF() {
        if (!selectedType) {
            toastStore.add('Please select a label type first', 'error');
            return;
        }
        loading = true;
        try {
            // Logic: In UI, tight mode simply means we might draw cut lines differently,
            // but margin logic remains.
            // For the backend, we pass the raw values.
            const requestBody = {
                type: selectedType,
                startNumber: parseInt(startNumber),
                count: parseInt(count),
                cols: parseInt(cols),
                rows: parseInt(rows),
                marginTop: marginTop,
                marginBottom: marginBottom,
                marginLeft: marginLeft,
                marginRight: marginRight,
                gapX: gapX,
                gapY: gapY,
                isTightMode: isTightMode,
                serialDigits: serialDigits,
                contentConfig: styleCfg,
                rackId: selectedRack || null
            };

            if (selectedType === 'p') {
                requestBody.warehouseConfig = warehouseConfig;
            }

            const token = localStorage.getItem('auth_token');
            const response = await fetch('/api/print/labels', {
                method: 'POST',
                headers: {
                    'Content-Type': 'application/json',
                    'Authorization': `Bearer ${token}`
                },
                body: JSON.stringify(requestBody)
            });

            if (!response.ok) {
                const err = await response.text();
                throw new Error(err || 'Failed to generate PDF');
            }

            const blob = await response.blob();
            const url = window.URL.createObjectURL(blob);
            const a = document.createElement('a');
            a.href = url;
            a.download = `labels_${selectedType}_${startNumber}.pdf`;
            document.body.appendChild(a);
            a.click();
            window.URL.revokeObjectURL(url);
            document.body.removeChild(a);
            toastStore.add('Labels generated successfully!', 'success');
        } catch (e) {
            console.error(e);
            toastStore.add(e.message, 'error');
        } finally {
            loading = false;
        }
    }

    onMount(() => {
        // Load print settings from localStorage
        const saved = localStorage.getItem('print_settings');
        if (saved) {
            try {
                const s = JSON.parse(saved);
                if (s.marginTop !== undefined) marginTop = parseFloat(s.marginTop);
                if (s.marginBottom !== undefined) marginBottom = parseFloat(s.marginBottom);
                if (s.marginLeft !== undefined) marginLeft = parseFloat(s.marginLeft);
                if (s.marginRight !== undefined) marginRight = parseFloat(s.marginRight);
                if (s.gapX !== undefined) gapX = parseFloat(s.gapX);
                if (s.gapY !== undefined) gapY = parseFloat(s.gapY);
                if (s.cols !== undefined) cols = parseInt(s.cols);
                if (s.rows !== undefined) rows = parseInt(s.rows);
                if (s.selectedType !== undefined) selectedType = s.selectedType;
                if (s.isTightMode !== undefined) isTightMode = s.isTightMode;
            } catch (e) {}
        }

        // Load content config only if a type was previously selected
        if (selectedType) {
            loadConfig(selectedType, cols, rows);

            // Load racks if Places type is selected
            if (selectedType === 'p') {
                loadRacks();
            }
        }
    });

    // Auto-save print settings (margins, gaps, layout, type, mode)
    $: if (typeof window !== 'undefined') {
        localStorage.setItem('print_settings', JSON.stringify({
            marginTop, marginBottom, marginLeft, marginRight, gapX, gapY, cols, rows,
            selectedType, isTightMode
        }));
    }

    // Auto-save content config when styleCfg or serialDigits changes
    $: if (typeof window !== 'undefined' && selectedType && styleCfg) {
        // Trigger save when any element config changes
        const trigger = [styleCfg.qr1, styleCfg.qr2, styleCfg.qr3, styleCfg.checksum, styleCfg.serial, serialDigits];
        saveConfig(selectedType, cols, rows);
    }
</script>

<div class="print-page">
    <header>
        <h1>Printing Center</h1>
    </header>

    <!-- Page Layout Editor -->
    <div class="card">
        <h2>Page Layout (A4)</h2>

        <!-- Quick Controls -->
        <div class="config-bar">
            <div class="field-group">
                <label>Columns</label>
                <input type="number" bind:value={cols} min="1" max="10" on:change={onLayoutChange} />
            </div>
            <div class="field-group">
                <label>Rows</label>
                <input type="number" bind:value={rows} min="1" max="20" on:change={onLayoutChange} />
            </div>
            <div class="field-group">
                <label>Count</label>
                <input type="number" bind:value={count} />
            </div>
            <div class="field-group">
                <label>Start #</label>
                <input type="number" bind:value={startNumber} />
            </div>
            <div class="toggle-group">
                <label>
                    <input type="checkbox" bind:checked={isTightMode} />
                    Tight Mode (Overlap)
                </label>
            </div>
        </div>

        <!-- The Visual Editor -->
        <div class="visual-editor-container" class:is-safe={!isTightMode}>
            <!-- Uses CSS Variables to map real millimeters to pixels in the preview -->
            <!-- 210mm / 297mm aspect ratio is maintained by CSS -->
            <div
                class="visual-page"
                style="
                    --mt: {marginTop}px;
                    --mb: {marginBottom}px;
                    --ml: {marginLeft}px;
                    --mr: {marginRight}px;
                    --gx: {gapX}px;
                    --gy: {gapY}px;
                    --cols: {cols};
                    --rows: {rows};
                "
            >
                <!-- Margin Inputs (Absolute positioned relative to page) -->
                <div class="margin-control top">
                    <input type="number" bind:value={marginTop} min="0" />
                    <span>Top</span>
                </div>
                <div class="margin-control left">
                    <input type="number" bind:value={marginLeft} min="0" />
                    <span>Left</span>
                </div>
                <div class="margin-control right">
                    <input type="number" bind:value={marginRight} min="0" />
                    <span>Right</span>
                </div>
                <div class="margin-control bottom">
                    <input type="number" bind:value={marginBottom} min="0" />
                    <span>Bottom</span>
                </div>

                <!-- Grid Container (Lives inside margins) -->
                <div class="grid-area">
                    <!-- Generate grid items dynamically -->
                    {#each Array(cols * rows) as _, i}
                        <div class="label-cell">
                            <div class="label-content">
                                <span class="lbl-text">L{i+1}</span>
                            </div>
                        </div>
                    {/each}

                    <!-- Gap controls outside of cells, positioned absolutely -->
                    {#if cols > 1}
                        <div class="gap-handle x">
                            <span>↔</span>
                            <input type="number" bind:value={gapX} min="0" max="50" step="0.5" />
                        </div>
                    {/if}
                    {#if rows > 1}
                        <div class="gap-handle y">
                            <span>↕</span>
                            <input type="number" bind:value={gapY} min="0" max="50" step="0.5" />
                        </div>
                    {/if}
                </div>
            </div>
        </div>

        <div class="dims-info">
            Label Size: <strong>{labelDims.w} x {labelDims.h} mm</strong>
        </div>
    </div>

    <!-- Label Type Selection -->
    <div class="card">
        <h2>Select Template</h2>
        <div class="type-grid">
            <button class="type-card" class:active={selectedType === 'i'} on:click={() => selectType('i')}>
                <h3>Items</h3>
            </button>
            <button class="type-card" class:active={selectedType === 'b'} on:click={() => selectType('b')}>
                <h3>Boxes</h3>
            </button>
            <button class="type-card" class:active={selectedType === 'p'} on:click={() => selectType('p')}>
                <h3>Places</h3>
            </button>
            <button class="type-card" class:active={selectedType === 'l'} on:click={() => selectType('l')}>
                <h3>Labels</h3>
            </button>
        </div>
    </div>

    <!-- Warehouse Config (for Places) -->
    {#if selectedType === 'p'}
    <div class="card">
        <div class="card-header">
            <h2>Warehouse Locations</h2>
            <button class="btn-sm" on:click={() => showPlanner = !showPlanner}>
                {showPlanner ? 'Print Mode' : 'Planner'}
            </button>
        </div>

        {#if !showPlanner}
        <div class="rack-select">
            <label>Select Rack:</label>
            <select bind:value={selectedRack} on:change={onRackSelect}>
                <option value="">-- Manual Configuration --</option>
                {#each registeredRacks as rack}
                <option value={rack.id}>
                    {rack.name} ({rack.columns}x{rack.rows}, ID: {rack.start_index}+)
                </option>
                {/each}
            </select>
        </div>
        {:else}
        <div class="planner">
            <table class="rack-table">
                <thead><tr><th>Name</th><th>Size</th><th>Range</th></tr></thead>
                <tbody>
                    {#each registeredRacks as rack}
                    <tr>
                        <td>{rack.name}</td>
                        <td>{rack.columns} x {rack.rows}</td>
                        <td>{rack.start_index} - {rack.start_index + rack.columns * rack.rows - 1}</td>
                    </tr>
                    {/each}
                </tbody>
            </table>
        </div>
        {/if}
    </div>
    {/if}

    <!-- Content Styling -->
    <div class="card">
        <h2>Content Positioning</h2>
        <div class="styling-layout">
            <div class="preview-box">
                <h3 style="color: #888; font-size: 0.9rem; margin: 0;">Live Preview ({layoutKey})</h3>
                <!-- Single Label Preview -->
                <div class="label-preview" style="aspect-ratio: {labelDims.aspect};">
                    {#if selectedType}
                        <div class="pv-element pv-qr" class:selected={selectedElement === 'qr1'}
                            style="left: {styleCfg.qr1.x}%; bottom: {styleCfg.qr1.y}%; width: {styleCfg.qr1.scale * 60}px; height: {styleCfg.qr1.scale * 60}px;"
                            on:click={() => selectedElement = 'qr1'}>QR1</div>
                        <div class="pv-element pv-qr" class:selected={selectedElement === 'qr2'}
                            style="left: {styleCfg.qr2.x}%; bottom: {styleCfg.qr2.y}%; width: {styleCfg.qr2.scale * 60}px; height: {styleCfg.qr2.scale * 60}px;"
                            on:click={() => selectedElement = 'qr2'}>QR2</div>
                        <div class="pv-element pv-qr" class:selected={selectedElement === 'qr3'}
                            style="left: {styleCfg.qr3.x}%; bottom: {styleCfg.qr3.y}%; width: {styleCfg.qr3.scale * 60}px; height: {styleCfg.qr3.scale * 60}px;"
                            on:click={() => selectedElement = 'qr3'}>QR3</div>
                        <div class="pv-element pv-text" class:selected={selectedElement === 'checksum'}
                            style="left: {styleCfg.checksum.x}%; bottom: {styleCfg.checksum.y}%; font-size: {styleCfg.checksum.scale * 30}px;"
                            on:click={() => selectedElement = 'checksum'}>XX</div>
                        <div class="pv-element pv-text serial" class:selected={selectedElement === 'serial'}
                            style="left: {styleCfg.serial.x}%; bottom: {styleCfg.serial.y}%; font-size: {styleCfg.serial.scale * 30}px;"
                            on:click={() => selectedElement = 'serial'}>123456</div>
                    {:else}
                        <div style="position: absolute; top: 50%; left: 50%; transform: translate(-50%, -50%); color: #888; text-align: center;">
                            <p style="margin: 0; font-size: 0.9rem;">Select a label type above</p>
                        </div>
                    {/if}
                </div>
                <p class="preview-hint">Actual size: {labelDims.w} x {labelDims.h} mm</p>
            </div>

            <div class="styling-controls">
                {#if selectedType}
                    <div class="control-group">
                        <label>Element</label>
                        <select bind:value={selectedElement}>
                            <option value="qr1">QR1 (Main)</option>
                            <option value="qr2">QR2</option>
                            <option value="qr3">QR3</option>
                            <option value="checksum">Checksum</option>
                            <option value="serial">Serial</option>
                        </select>
                    </div>
                    <div class="control-row">
                        <div class="c-item"><label>X%</label><input type="number" bind:value={styleCfg[selectedElement].x} min="-50" max="150" step="0.5"/></div>
                        <div class="c-item"><label>Y%</label><input type="number" bind:value={styleCfg[selectedElement].y} min="-50" max="150" step="0.5"/></div>
                    </div>
                    <div class="control-group">
                        <label>Scale (0.05 - 2.0)</label>
                        <input type="number" bind:value={styleCfg[selectedElement].scale} step="0.05" min="0.05" max="2" />
                    </div>
                    <div class="control-group">
                        <label>Serial Digits (0 = all)</label>
                        <input type="number" bind:value={serialDigits} min="0" max="18" title="Show last N digits of serial. 0 = all digits" />
                    </div>
                    <button class="btn-sm" on:click={resetToDefault}>Reset to Default</button>
                {:else}
                    <div style="padding: 40px 20px; text-align: center; color: #666;">
                        <p style="margin: 0; font-size: 0.9rem;">Select a label type to customize positioning</p>
                    </div>
                {/if}
            </div>
        </div>
    </div>

    <!-- Generate -->
    <div class="actions">
        <button class="btn primary large" on:click={generatePDF} disabled={loading || !selectedType}>
            {loading ? 'Generating...' : 'Generate PDF'}
        </button>
    </div>
</div>

<style>
    .print-page { max-width: 900px; margin: 0 auto; padding-bottom: 3rem; }
    header { margin-bottom: 1rem; }
    h1 { color: #fff; font-size: 1.5rem; margin: 0; }
    h2 { color: #5a7ba9; font-size: 0.9rem; text-transform: uppercase; letter-spacing: 1px; margin: 0 0 1rem 0; border-bottom: 1px solid #333; padding-bottom: 5px; }

    .card { background: #1e1e1e; border: 1px solid #333; border-radius: 6px; padding: 1rem; margin-bottom: 1rem; }

    /* Config Bar */
    .config-bar { display: flex; flex-wrap: wrap; gap: 1rem; margin-bottom: 1rem; align-items: flex-end; }
    .field-group label { display: block; font-size: 0.75rem; color: #888; margin-bottom: 2px; }
    .field-group input { width: 60px; background: #111; border: 1px solid #444; color: #fff; padding: 4px; border-radius: 3px; text-align: center; }
    .toggle-group { display: flex; align-items: center; height: 30px; font-size: 0.8rem; color: #ccc; }
    .toggle-group input { margin-right: 5px; }

    /* Visual Editor */
    .visual-editor-container {
        display: flex;
        justify-content: center;
        background: #252525;
        padding: 20px;
        border-radius: 4px;
        overflow: auto;
    }

    .visual-page {
        /* Define A4 aspect ratio 210mm x 297mm */
        width: 300px;
        aspect-ratio: 210 / 297;
        background: #d1d1d1;
        position: relative;
        box-shadow: 0 5px 15px rgba(0,0,0,0.5);
        /* Critical: Use padding for margins */
        padding-top: var(--mt);
        padding-bottom: var(--mb);
        padding-left: var(--ml);
        padding-right: var(--mr);
        box-sizing: border-box; /* Padding is included in width, but we want padding to shrink content */
    }

    /* Grid Area */
    .grid-area {
        width: 100%;
        height: 100%;
        display: grid;
        position: relative;
        /* Use dynamic CSS variables for columns/rows */
        grid-template-columns: repeat(var(--cols), 1fr);
        grid-template-rows: repeat(var(--rows), 1fr);
        /* Use gaps */
        gap: var(--gy) var(--gx);
        /* Background hint for margins area */
        background: rgba(255,255,255,0.2);
        border: 1px dashed rgba(0,0,0,0.1);
    }

    .label-cell {
        background: #5a7ba9;
        border-radius: 2px;
        position: relative;
        display: flex;
        align-items: center;
        justify-content: center;
        z-index: 1;
        transition: all 0.4s cubic-bezier(0.175, 0.885, 0.32, 1.275);
        /* In Tight Mode, labels stretch to fill their cell completely */
        margin: 0;
    }

    /* Label margin/padding visualization (lighter border around label) */
    /* This shows the physical label margin that extends beyond printable area */
    .label-cell::after {
        content: '';
        position: absolute;
        top: calc(var(--gy) / -2);
        left: calc(var(--gx) / -2);
        right: calc(var(--gx) / -2);
        bottom: calc(var(--gy) / -2);
        background: rgba(90, 123, 169, 0.2);
        border: 1px dashed rgba(90, 123, 169, 0.4);
        border-radius: 4px;
        z-index: -1;
        pointer-events: none;
        transition: all 0.4s cubic-bezier(0.175, 0.885, 0.32, 1.275);
    }

    /* First label cell has higher z-index to keep gap controls on top */
    .label-cell:first-child {
        z-index: 5;
    }

    /* In Safe Mode (Overlap OFF), labels shrink and their margin stays inside */
    .visual-editor-container.is-safe .label-cell {
        /* Shrink labels by adding margin to keep physical margin inside cell */
        margin: calc(var(--gy) / 2) calc(var(--gx) / 2);
    }

    /* In Safe Mode, the ::after margin indicator should be contained within the cell */
    /* These simplify to 0, meaning the margin stays at the edge of the label */
    .visual-editor-container.is-safe .label-cell::after {
        top: 0;
        left: 0;
        right: 0;
        bottom: 0;
    }

    .label-content {
        color: rgba(255,255,255,0.8);
        font-size: 10px;
        font-weight: bold;
    }

    /* Margin Controls (Floating) */
    .margin-control {
        position: absolute;
        background: white;
        border: 1px solid #991b1b;
        border-radius: 3px;
        padding: 1px 4px;
        display: flex;
        align-items: center;
        gap: 2px;
        z-index: 10;
        box-shadow: 0 2px 4px rgba(0,0,0,0.2);
    }
    .margin-control input {
        width: 30px; border: none; text-align: center; color: #991b1b; font-weight: bold; font-size: 11px;
    }
    .margin-control span { font-size: 9px; color: #991b1b; text-transform: uppercase; }

    .margin-control.top { top: -12px; left: 50%; transform: translateX(-50%); }
    .margin-control.bottom { bottom: -12px; left: 50%; transform: translateX(-50%); }
    .margin-control.left { left: -30px; top: 50%; transform: translateY(-50%) rotate(-90deg); }
    .margin-control.right { right: -30px; top: 50%; transform: translateY(-50%) rotate(90deg); }

    /* Gap Handles */
    .gap-handle {
        position: absolute;
        background: #f59e0b;
        color: #78350f;
        font-size: 9px;
        padding: 2px 4px;
        border-radius: 3px;
        white-space: nowrap;
        z-index: 10;
        display: flex;
        align-items: center;
        gap: 3px;
        box-shadow: 0 2px 4px rgba(0,0,0,0.3);
    }
    .gap-handle span {
        font-weight: bold;
    }
    .gap-handle input {
        width: 32px;
        padding: 2px 3px;
        border: 1px solid #78350f;
        background: rgba(254, 243, 199, 0.95);
        color: #78350f;
        border-radius: 2px;
        font-size: 10px;
        text-align: center;
        font-weight: bold;
    }
    /* Position gap X between first and second column */
    .gap-handle.x {
        left: calc(100% / var(--cols) + var(--gx) / 2);
        top: calc(100% / var(--rows) / 2);
        transform: translate(-50%, -50%);
    }
    /* Position gap Y between first and second row */
    .gap-handle.y {
        left: calc(100% / var(--cols) / 2);
        top: calc(100% / var(--rows) + var(--gy) / 2);
        transform: translate(-50%, -50%);
    }

    .dims-info { text-align: center; margin-top: 10px; color: #888; font-size: 0.9rem; }

    /* Type Selector */
    .type-grid { display: grid; grid-template-columns: repeat(4, 1fr); gap: 10px; }
    .type-card {
        background: #252525; border: 1px solid #444; border-radius: 4px; padding: 10px; cursor: pointer; text-align: center;
    }
    .type-card.active { border-color: #5a7ba9; background: rgba(90, 123, 169, 0.2); }
    .type-card h3 { margin: 0; color: #ccc; font-size: 0.9rem; }

    /* Content Styling Preview */
    .styling-layout { display: grid; grid-template-columns: 1fr 1fr; gap: 20px; }
    .preview-box { display: flex; flex-direction: column; justify-content: center; align-items: center; background: #333; padding: 20px; border-radius: 4px; gap: 10px; }
    .label-preview {
        width: 100%;
        max-width: 300px;
        background: #ddd;
        position: relative;
        border-radius: 3px;
        min-height: 120px;
    }
    .preview-hint {
        color: #888;
        font-size: 0.85rem;
        text-align: center;
    }
    .pv-element {
        position: absolute; border: 1px dashed #5a7ba9; display: flex; align-items: center; justify-content: center;
        color: #5a7ba9; font-size: 10px; cursor: pointer;
    }
    .pv-element:hover { background: rgba(90, 123, 169, 0.2); }
    .pv-element.selected { border: 2px solid #5a7ba9; background: rgba(90, 123, 169, 0.1); z-index: 20 !important; }
    .pv-qr { background: rgba(255,255,255,0.5); z-index: 10; }
    .pv-text { white-space: nowrap; font-family: monospace; z-index: 5; }

    .styling-controls { display: flex; flex-direction: column; gap: 10px; }
    .control-group label { display: block; font-size: 0.75rem; color: #888; margin-bottom: 2px; }
    .control-group select, .control-group input { width: 100%; background: #111; border: 1px solid #444; color: #fff; padding: 5px; }
    .control-row { display: flex; gap: 10px; }
    .c-item { flex: 1; }
    .c-item input { width: 100%; }

    .btn { padding: 10px 20px; background: #5a7ba9; color: white; border: none; border-radius: 4px; cursor: pointer; font-weight: bold; }
    .btn:disabled { opacity: 0.5; }
    .btn-sm { padding: 5px 10px; background: #333; color: #ccc; border: none; border-radius: 3px; cursor: pointer; }
    .actions { display: flex; justify-content: center; margin-top: 20px; }

    /* Warehouse */
    .card-header { display: flex; justify-content: space-between; align-items: center; }
    .rack-select { margin: 10px 0; }
    .rack-select label { display: block; color: #888; font-size: 0.9rem; margin-bottom: 5px; }
    .rack-select select { width: 100%; padding: 8px; background: #111; border: 1px solid #444; color: #fff; border-radius: 4px; }
    .rack-table { width: 100%; border-collapse: collapse; }
    .rack-table th, .rack-table td { padding: 8px; text-align: left; border-bottom: 1px solid #333; }
    .rack-table th { color: #888; font-size: 0.8rem; text-transform: uppercase; }
</style>
