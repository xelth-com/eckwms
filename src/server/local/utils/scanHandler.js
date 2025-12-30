const inventoryService = require('../services/inventoryService');
const {
    findKnownCode,
    isBetDirect,
    disAct,
    toAct,
    isAct
} = require('./dataInit');
const { eckUrlDecrypt } = require('../../../shared/utils/encryption');
const geminiService = require('../services/geminiService');
const { searchInventoryTool, linkCodeTool } = require('../tools/inventoryTools');
const { AGENT_SYSTEM_PROMPT } = require('../prompts/agentPrompt');

// Buffers remain in memory for session context
let iTem = [];
let bOx = [];
let pLace = [];

function unixTime() {
    return Math.floor(Date.now() / 1000);
}

async function writeLog(str) {
    try {
        const { appendFile } = require('fs/promises');
        const { resolve } = require('path');
        const dateTemp = new Date();
        const logDir = resolve('./logs');
        const fs = require('fs');
        if (!fs.existsSync(logDir)) fs.mkdirSync(logDir, { recursive: true });
        const filename = `${dateTemp.getFullYear().toString().slice(-2)}${('0' + (dateTemp.getMonth() + 1)).slice(-2)}.txt`;
        await appendFile(resolve(logDir, filename), `${str}\t${new Date().toISOString()}\n`);
    } catch (e) { console.error('Log error:', e); }
}

async function processScan(barcode, user = null) {
    let result = {
        type: 'unknown',
        message: '',
        data: {},
        buffers: { items: [...iTem], boxes: [...bOx], places: [...pLace] }
    };

    try {
        let bet = '';
        if (barcode.startsWith('http')) {
            const code = barcode.split('/').pop();
            bet = findKnownCode(code) || isBetDirect(code);
        } else if (barcode.startsWith('ECK') && barcode.length === 76) {
            try { bet = eckUrlDecrypt(barcode); } catch (e) {}
        }
        if (!bet) bet = findKnownCode(barcode) || isBetDirect(barcode);

        if (bet) {
            const type = bet.slice(0, 1);
            switch (type) {
                case 'i': result = await handleItemBarcode(bet); break;
                case 'b': result = await handleBoxBarcode(bet); break;
                case 'p': result = await handlePlaceBarcode(bet); break;
                // Order and User handlers can be migrated later or kept if using global.orders
                default: result.message = `Unknown barcode type: ${type}`;
            }
        } else {
            result = await handleUnknownBarcode(barcode);
        }

        result.buffers = { items: [...iTem], boxes: [...bOx], places: [...pLace] };
        return result;
    } catch (error) {
        console.error('Scan Error:', error);
        return { type: 'error', message: error.message, data: {}, buffers: result.buffers };
    }
}

async function handleItemBarcode(id) {
    const exists = await inventoryService.exists('item', id);
    let message = '';
    let data = {};

    if (!exists) {
        // Create new item
        const newItem = { sn: [id, unixTime()], cl: null, actn: [] };
        await inventoryService.create('item', id, newItem);
        message = 'New item created';
    } else {
        const item = await inventoryService.get('item', id);
        data = { serialNumber: id, created: item.sn[1], class: item.cl || 'Unknown' };
        message = 'Item found';
    }

    // Update Buffer
    updateBuffer(iTem, id);
    if (iTem.includes(toAct(id))) message = 'Item activated';
    else message = 'Item deactivated/added';

    return { type: 'item', message, data };
}

async function handleBoxBarcode(id) {
    const exists = await inventoryService.exists('box', id);
    let message = '';
    let data = {};

    if (!exists) {
        const newBox = { sn: [id, unixTime()], cont: [], loc: [] };
        await inventoryService.create('box', id, newBox);
        message = 'New box created';
    } else {
        const box = await inventoryService.get('box', id);
        data = { serialNumber: id, contents: box.cont || [] };
        message = 'Box found';
    }

    // Logic: Put active items into this box
    if (iTem.length > 0) {
        for (const itemId of iTem) {
            const cleanId = disAct(itemId);
            // Update Item location
            await inventoryService.pushToArray('item', cleanId, 'loc', [id, unixTime()]);
            // Update Box content
            await inventoryService.pushToArray('box', id, 'cont', [cleanId, unixTime()]);
        }
        message = `${iTem.length} items added to box ${id}`;
        await writeLog(`[${iTem.join(', ')}] => ${id}`);
        iTem.length = 0; // Clear item buffer
    }

    updateBuffer(bOx, id);
    return { type: 'box', message, data };
}

async function handlePlaceBarcode(id) {
    // Placeholder for Place logic (similar to Box)
    const exists = await inventoryService.exists('place', id);
    if (!exists) {
        await inventoryService.create('place', id, { sn: [id, unixTime()], cont: [] });
    }

    let message = 'Place scanned';
    // If boxes are active, move them to this place
    if (bOx.length > 0) {
        for (const boxId of bOx) {
            const cleanId = disAct(boxId);
            await inventoryService.pushToArray('box', cleanId, 'loc', [id, unixTime()]);
            await inventoryService.pushToArray('place', id, 'cont', [cleanId, unixTime()]);
        }
        message = `${bOx.length} boxes moved to place ${id}`;
        await writeLog(`[${bOx.join(', ')}] => ${id}`);
        bOx.length = 0;
    }

    updateBuffer(pLace, id);
    return { type: 'place', message, data: {} };
}

function updateBuffer(buffer, id) {
    const activeId = toAct(id);
    const index = buffer.indexOf(activeId);
    if (index > -1) {
        // If already active, maybe toggle? For now, we keep simpler logic from original:
        // Actually original logic toggles state. Let's simplify: if active, deactivate. If not, activate.
        // But here we'll just push to end if not present, or remove if present (toggle).
         buffer.splice(index, 1); // Remove
    } else {
         buffer.push(activeId); // Add
    }
}

// Fallback for Unknown Barcodes (AI)
async function handleUnknownBarcode(barcode) {
    // Simplification: just return unknown for now, or hook up AI logic similarly to before
    // For this migration step, we ensure DB logic works first.
    return { type: 'unknown', message: `Unknown barcode: ${barcode}` };
}

module.exports = { processScan };
