// utils/scanHandler.js
const inventoryService = require('../services/inventoryService');
const { eckUrlDecrypt } = require('../../../shared/utils/encryption');
const { writeLog } = require('./fileUtils'); // Assuming simple file logger or replace with DB logging

// Session Buffers (In-Memory for active session context)
let iTem = [];
let bOx = [];
let pLace = [];

function unixTime() { return Math.floor(Date.now() / 1000); }

// Helper to identify code type
function identifyCode(code) {
    if (code.startsWith('i7')) return 'item';
    if (code.startsWith('b')) return 'box';
    if (code.startsWith('p')) return 'place';
    if (code.startsWith('l')) return 'marker';
    return 'unknown';
}

async function processScan(barcode, user = null) {
    let result = {
        type: 'unknown',
        message: '',
        data: {},
        buffers: { items: [...iTem], boxes: [...bOx], places: [...pLace] }
    };

    try {
        let cleanCode = barcode;
        // Decrypt if ECK format
        if (barcode.startsWith('ECK') && barcode.length === 76) {
            const decrypted = eckUrlDecrypt(barcode);
            if (decrypted) cleanCode = decrypted;
        } else if (barcode.startsWith('http')) {
            cleanCode = barcode.split('/').pop();
        }

        const type = identifyCode(cleanCode);

        if (type === 'item') {
            result = await handleItem(cleanCode);
        } else if (type === 'box') {
            result = await handleBox(cleanCode);
        } else if (type === 'place') {
            result = await handlePlace(cleanCode);
        } else {
            // Unknown / External code
            // TODO: Hook up AI Service here for Product Alias check
            result.message = `Unknown code: ${cleanCode}`;
            result.type = 'unknown';
        }

        result.buffers = { items: [...iTem], boxes: [...bOx], places: [...pLace] };
        return result;

    } catch (error) {
        console.error('Scan Error:', error);
        return { type: 'error', message: error.message, data: {}, buffers: result.buffers };
    }
}

async function handleItem(id) {
    const exists = await inventoryService.exists('item', id);
    let message = '';

    if (!exists) {
        await inventoryService.create('item', id, { sn: [id, unixTime()], actn: [] });
        message = 'New Item Created';
    } else {
        message = 'Item Scanned';
    }

    // Toggle buffer
    const idx = iTem.indexOf(id);
    if (idx > -1) {
        iTem.splice(idx, 1);
        message += ' (Deselected)';
    } else {
        iTem.push(id);
        message += ' (Selected)';
    }

    const item = await inventoryService.get('item', id);
    return { type: 'item', message, data: { id, created: item.sn[1] } };
}

async function handleBox(id) {
    const exists = await inventoryService.exists('box', id);
    let message = '';

    if (!exists) {
        await inventoryService.create('box', id, { sn: [id, unixTime()], cont: [], loc: [] });
        message = 'New Box Created';
    } else {
        message = 'Box Scanned';
    }

    // Move active items into box
    if (iTem.length > 0) {
        for (const itemId of iTem) {
            await inventoryService.pushToArray('item', itemId, 'loc', [id, unixTime()]);
            await inventoryService.pushToArray('box', id, 'cont', [itemId, unixTime()]);
        }
        message = `${iTem.length} items moved to box ${id}`;
        iTem = []; // Clear items
    }

    // Toggle box buffer
    const idx = bOx.indexOf(id);
    if (idx > -1) bOx.splice(idx, 1);
    else bOx.push(id);

    const box = await inventoryService.get('box', id);
    return { type: 'box', message, data: { id, count: box.cont?.length || 0 } };
}

async function handlePlace(id) {
    const exists = await inventoryService.exists('place', id);
    if (!exists) {
        await inventoryService.create('place', id, { sn: [id, unixTime()] });
    }

    let message = 'Place Scanned';
    // Move boxes to place
    if (bOx.length > 0) {
        for (const boxId of bOx) {
            await inventoryService.pushToArray('box', boxId, 'loc', [id, unixTime()]);
            await inventoryService.pushToArray('place', id, 'cont', [boxId, unixTime()]);
        }
        message = `${bOx.length} boxes moved to place ${id}`;
        bOx = [];
    }

    return { type: 'place', message, data: { id } };
}

module.exports = { processScan };
