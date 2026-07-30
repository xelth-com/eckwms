<script>
    import { onMount, tick } from 'svelte';
    import { api } from '$lib/api';
    import { base } from '$app/paths';
    import { toastStore } from '$lib/stores/toastStore';
    import { t, tr } from '$lib/i18n';

    // A learn-by-scan stocktake: pick a warehouse, scan a shelf to open its bin,
    // then scan parts. In "Count" mode each item scan adds +step (scan-gun flow);
    // "Set" writes the absolute counted qty. Everything drives POST
    // /api/warehouse/put-away — bins are minted on first scan.
    let warehouse = 'WH008';
    let mode = 'count'; // 'count' (+step per scan) | 'set' (absolute)
    let step = 1;

    let shelfInput = '';
    let shelf = ''; // currently open shelf barcode
    let binLoc = null; // location record (null until first scan creates it)
    let binLines = []; // quant lines on the shelf

    let itemInput = '';
    let busy = false;
    let log = []; // recent session activity, newest first
    let itemEl;
    let shelfEl;

    onMount(async () => {
        await tick();
        shelfEl?.focus();
    });

    async function openShelf() {
        const code = shelfInput.trim();
        if (!code) return;
        shelf = code;
        await loadBin();
        await tick();
        itemEl?.focus();
    }

    function closeShelf() {
        shelf = '';
        shelfInput = '';
        binLoc = null;
        binLines = [];
        tick().then(() => shelfEl?.focus());
    }

    async function loadBin() {
        if (!shelf) {
            binLoc = null;
            binLines = [];
            return;
        }
        try {
            const res = await api.get(`/api/warehouse/bin?barcode=${encodeURIComponent(shelf)}`);
            binLoc = res.location || null;
            binLines = res.lines || [];
        } catch (e) {
            toastStore.add(tr('warehouse.bin_load_failed', { error: e.message }), 'error');
        }
    }

    async function scanItem() {
        const item = itemInput.trim();
        if (!item) return;
        if (!shelf) {
            toastStore.add(tr('warehouse.open_shelf_first'), 'warning');
            shelfEl?.focus();
            return;
        }
        busy = true;
        try {
            const res = await api.post('/api/warehouse/put-away', {
                item_barcode: item,
                shelf_barcode: shelf,
                warehouse: warehouse.trim(),
                qty: Number(step) || 1,
                op: mode === 'count' ? 'add' : 'set'
            });
            const p = res.placed;
            log = [
                {
                    t: new Date().toLocaleTimeString(),
                    item: p.product.default_code || item,
                    name: p.product.name || '',
                    action: p.quant.op,
                    qty: p.quant.qty,
                    ok: true
                },
                ...log
            ].slice(0, 60);
            itemInput = '';
            await loadBin();
        } catch (e) {
            log = [
                { t: new Date().toLocaleTimeString(), item, name: '', action: 'error', qty: '', ok: false, err: e.message },
                ...log
            ].slice(0, 60);
            toastStore.add(e.message, 'error');
        } finally {
            busy = false;
            await tick();
            itemEl?.focus();
        }
    }

    $: binTotal = binLines.reduce((s, l) => s + (Number(l.qty) || 0), 0);
</script>

<div class="stocktake">
    <header>
        <div class="title">
            <a href="{base}/dashboard/warehouse" class="back">← {$t('warehouse.back_warehouse')}</a>
            <h1>{$t('warehouse.stocktake_title')}</h1>
        </div>
        <div class="controls">
            <label>
                {$t('warehouse.field_warehouse')}
                <input class="wh" bind:value={warehouse} placeholder="WH008" />
            </label>
            <div class="mode-toggle" role="group" aria-label="mode">
                <button class:active={mode === 'count'} on:click={() => (mode = 'count')}>{$t('warehouse.mode_count', { step })}</button>
                <button class:active={mode === 'set'} on:click={() => (mode = 'set')}>{$t('warehouse.mode_set')}</button>
            </div>
            <label>
                {mode === 'count' ? $t('warehouse.step') : $t('warehouse.qty')}
                <input class="step" type="number" min="1" bind:value={step} />
            </label>
        </div>
    </header>

    <!-- Shelf selector -->
    {#if !shelf}
        <div class="shelf-open card">
            <div class="hint">{$t('warehouse.shelf_hint')}</div>
            <div class="row">
                <input
                    bind:this={shelfEl}
                    bind:value={shelfInput}
                    placeholder={$t('warehouse.placeholder_shelf')}
                    on:keydown={(e) => e.key === 'Enter' && openShelf()}
                />
                <button class="primary" on:click={openShelf}>{$t('warehouse.open_bin')}</button>
            </div>
        </div>
    {:else}
        <div class="active-shelf card">
            <div class="shelf-badge">
                <span class="lbl">{$t('warehouse.shelf_label')}</span>
                <span class="code">{shelf}</span>
                {#if binLoc}<span class="loc">{binLoc.complete_name || ''}</span>{:else}<span class="new">{$t('warehouse.new_bin')}</span>{/if}
            </div>
            <button class="close" on:click={closeShelf}>{$t('warehouse.change_shelf')}</button>
        </div>

        <div class="grid">
            <!-- Scanning + bin contents -->
            <section class="card scan-col">
                <div class="row">
                    <input
                        bind:this={itemEl}
                        bind:value={itemInput}
                        placeholder={$t('warehouse.placeholder_part')}
                        disabled={busy}
                        on:keydown={(e) => e.key === 'Enter' && scanItem()}
                    />
                    <button class="primary" on:click={scanItem} disabled={busy}>
                        {mode === 'count' ? `+${step}` : $t('warehouse.set_qty_btn', { step })}
                    </button>
                </div>

                <div class="bin-head">
                    <h3>{$t('warehouse.on_this_shelf')}</h3>
                    <span class="total">{$t('warehouse.bin_total', { lines: binLines.length, pcs: binTotal })}</span>
                </div>
                {#if binLines.length === 0}
                    <div class="empty">{$t('warehouse.empty_bin')}</div>
                {:else}
                    <table>
                        <thead>
                            <tr><th>{$t('warehouse.th_code')}</th><th>{$t('warehouse.th_name')}</th><th class="num">{$t('warehouse.th_qty')}</th></tr>
                        </thead>
                        <tbody>
                            {#each binLines as l}
                                <tr>
                                    <td class="mono">{l.default_code || '—'}</td>
                                    <td class="name">{l.product_name || ''}</td>
                                    <td class="num">{l.qty}</td>
                                </tr>
                            {/each}
                        </tbody>
                    </table>
                {/if}
            </section>

            <!-- Session log -->
            <section class="card log-col">
                <h3>{$t('warehouse.session')}</h3>
                {#if log.length === 0}
                    <div class="empty">{$t('warehouse.empty_session')}</div>
                {:else}
                    <ul class="log">
                        {#each log as e}
                            <li class:err={!e.ok}>
                                <span class="time">{e.t}</span>
                                <span class="mono">{e.item}</span>
                                {#if e.ok}
                                    <span class="act">{e.action}</span>
                                    <span class="q">→ {e.qty}</span>
                                    {#if e.name}<span class="nm">{e.name}</span>{/if}
                                {:else}
                                    <span class="act err-act">{$t('warehouse.log_error')}</span>
                                    <span class="nm">{e.err}</span>
                                {/if}
                            </li>
                        {/each}
                    </ul>
                {/if}
            </section>
        </div>
    {/if}
</div>

<style>
    .stocktake { max-width: 1100px; margin: 0 auto; color: #ddd; }
    header { display: flex; justify-content: space-between; align-items: flex-end; gap: 1rem; flex-wrap: wrap; margin-bottom: 1.25rem; }
    .title { display: flex; align-items: baseline; gap: 1rem; }
    .title h1 { margin: 0; color: #fff; font-size: 1.6rem; }
    .back { color: #7d97e0; text-decoration: none; font-size: 0.9rem; }
    .back:hover { color: #a3bffa; }

    .controls { display: flex; align-items: flex-end; gap: 1rem; flex-wrap: wrap; }
    .controls label { display: flex; flex-direction: column; gap: 0.25rem; font-size: 0.7rem; text-transform: uppercase; color: #888; letter-spacing: 0.5px; }
    .controls input { background: #1a1a1a; border: 1px solid #444; border-radius: 6px; color: #fff; padding: 0.5rem 0.7rem; font-size: 0.95rem; }
    .wh { width: 90px; }
    .step { width: 70px; }

    .mode-toggle { display: flex; border: 1px solid #444; border-radius: 6px; overflow: hidden; }
    .mode-toggle button { background: #1a1a1a; color: #aaa; border: none; padding: 0.55rem 0.9rem; cursor: pointer; font-size: 0.85rem; }
    .mode-toggle button.active { background: #4a69bd; color: #fff; }

    .card { background: #1e1e1e; border: 1px solid #333; border-radius: 10px; padding: 1.1rem 1.25rem; margin-bottom: 1rem; }
    .hint { color: #888; font-size: 0.85rem; margin-bottom: 0.75rem; }
    .row { display: flex; gap: 0.6rem; }
    .row input { flex: 1; background: #141414; border: 1px solid #444; border-radius: 6px; color: #fff; padding: 0.7rem 0.9rem; font-size: 1.05rem; }
    .row input:focus { outline: none; border-color: #4a69bd; }
    button.primary { background: #4a69bd; color: #fff; border: none; border-radius: 6px; padding: 0 1.2rem; font-weight: 600; cursor: pointer; font-size: 0.95rem; }
    button.primary:hover { background: #3d5aa8; }
    button.primary:disabled { opacity: 0.6; cursor: not-allowed; }

    .active-shelf { display: flex; justify-content: space-between; align-items: center; }
    .shelf-badge { display: flex; align-items: baseline; gap: 0.6rem; }
    .shelf-badge .lbl { font-size: 0.7rem; text-transform: uppercase; color: #777; }
    .shelf-badge .code { font-family: monospace; font-size: 1.2rem; color: #fff; font-weight: 700; }
    .shelf-badge .loc { color: #888; font-size: 0.85rem; }
    .shelf-badge .new { color: #4ade80; font-size: 0.75rem; background: #10331d; padding: 0.1rem 0.5rem; border-radius: 4px; }
    .close { background: #2a2a2a; color: #ccc; border: 1px solid #444; border-radius: 6px; padding: 0.45rem 0.9rem; cursor: pointer; font-size: 0.85rem; }
    .close:hover { background: #333; color: #fff; }

    .grid { display: grid; grid-template-columns: 1.3fr 1fr; gap: 1rem; align-items: start; }
    @media (max-width: 820px) { .grid { grid-template-columns: 1fr; } }

    .bin-head { display: flex; justify-content: space-between; align-items: baseline; margin: 1.1rem 0 0.5rem; }
    .bin-head h3, .log-col h3 { margin: 0; color: #fff; font-size: 1rem; }
    .total { color: #888; font-size: 0.8rem; }
    .empty { color: #666; font-size: 0.85rem; padding: 0.8rem 0; }

    table { width: 100%; border-collapse: collapse; }
    th, td { text-align: left; padding: 0.45rem 0.5rem; border-bottom: 1px solid #2a2a2a; font-size: 0.9rem; }
    th { color: #777; font-size: 0.7rem; text-transform: uppercase; }
    td.num, th.num { text-align: right; font-variant-numeric: tabular-nums; }
    .mono { font-family: monospace; color: #cbd5e1; }
    td.name { color: #aaa; max-width: 220px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

    ul.log { list-style: none; margin: 0.5rem 0 0; padding: 0; display: flex; flex-direction: column; gap: 0.35rem; max-height: 460px; overflow-y: auto; }
    ul.log li { display: flex; align-items: baseline; gap: 0.5rem; font-size: 0.85rem; padding: 0.35rem 0.5rem; background: #181818; border-radius: 5px; }
    ul.log li.err { background: #2a1414; }
    .time { color: #666; font-size: 0.72rem; font-variant-numeric: tabular-nums; }
    .act { color: #4ade80; font-size: 0.72rem; text-transform: uppercase; }
    .err-act { color: #ff6b6b; }
    .q { color: #fff; font-weight: 600; }
    .nm { color: #888; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
</style>
