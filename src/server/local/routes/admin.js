// routes/admin.js
const express = require('express');
const router = express.Router();
const { verifyJWT, betrugerUrlEncrypt, betrugerCrc } = require('../../../shared/utils/encryption');
const { addUnicEntryToProperty, addEntryToProperty } = require('../utils/dataInit');
const { generatePdfRma, betrugerPrintCodesPdf } = require('../utils/pdfGenerator');
const { syncPublicData } = require('../services/globalSyncService');
const { requireAdmin } = require('../middleware/auth');
const path = require('path');
const crc32 = require('buffer-crc32');
const { base32table } = require('../../../shared/utils/encryption');

// Serve the main admin dashboard (no auth required - auth handled client-side)
router.get('/', (req, res) => {
    res.sendFile(path.join(__dirname, '../views/admin/index.html'));
});

// Serve the device pairing page (no auth required - auth handled client-side)
router.get('/pairing', (req, res) => {
    res.sendFile(path.join(__dirname, '../views/admin/pairing.html'));
});

// Serve the printing center page (no auth required - auth handled client-side)
router.get('/printing', (req, res) => {
    res.sendFile(path.join(__dirname, '../views/admin/printing.html'));
});

// Admin dashboard (no auth required - auth handled client-side)
router.get('/dashboard', (req, res) => {
    res.sendFile(path.join(__dirname, '../html/admin-dashboard.html'));
});

// Apply authentication middleware to all API routes (not HTML pages)
router.use(requireAdmin);

// --- Device Management API ---

// Get list of all registered devices
router.get('/api/devices', async (req, res) => {
    try {
        // Import DB model inside handler to ensure connection exists
        const { RegisteredDevice } = require('../../../shared/models/postgresql');

        const devices = await RegisteredDevice.findAll({
            order: [
                ['status', 'DESC'], // Pending first (alphabetically Pending > Active)
                ['updatedAt', 'DESC']
            ]
        });
        res.json(devices);
    } catch (error) {
        console.error('Error fetching devices:', error);
        res.status(500).json({ error: error.message });
    }
});

// Update device status (Approve/Block)
router.post('/api/devices/:id/status', async (req, res) => {
    try {
        const { RegisteredDevice } = require('../../../shared/models/postgresql');
        const { status } = req.body;
        const { id } = req.params;

        if (!['active', 'pending', 'blocked'].includes(status)) {
            return res.status(400).json({ error: 'Invalid status' });
        }

        const device = await RegisteredDevice.findByPk(id);
        if (!device) {
            return res.status(404).json({ error: 'Device not found' });
        }

        device.status = status;
        await device.save();

        // --- REAL-TIME PUSH NOTIFICATION ---
        if (global.sendToDevice) {
            global.sendToDevice(device.deviceId, 'STATUS_UPDATE', {
                status: status,
                active: status === 'active',
                timestamp: Date.now()
            });
        }
        // -----------------------------------

        res.json({ success: true, device });
    } catch (error) {
        console.error('Error updating device:', error);
        res.status(500).json({ error: error.message });
    }
});

// Delete device
router.delete('/api/devices/:id', async (req, res) => {
    try {
        const { RegisteredDevice } = require('../../../shared/models/postgresql');
        const deleted = await RegisteredDevice.destroy({
            where: { deviceId: req.params.id }
        });
        res.json({ success: !!deleted });
    } catch (error) {
        res.status(500).json({ error: error.message });
    }
});

// Assign Role to Device and Push Permissions
router.post('/api/devices/:id/role', async (req, res) => {
    try {
        const { RegisteredDevice, Role, Permission } = require('../../../shared/models/postgresql');
        const { roleId } = req.body;
        const deviceId = req.params.id;

        const device = await RegisteredDevice.findByPk(deviceId);
        if (!device) return res.status(404).json({ error: 'Device not found' });

        const role = await Role.findByPk(roleId, {
            include: [{ model: Permission }]
        });
        if (!role) return res.status(404).json({ error: 'Role not found' });

        device.role_id = roleId;
        await device.save();

        // Extract permission slugs for the device
        const permissions = role.Permissions.map(p => p.slug);

        // --- REAL-TIME PUSH: ROLE UPDATE ---
        if (global.sendToDevice) {
            global.sendToDevice(deviceId, 'ROLE_UPDATE', {
                role: role.name,
                permissions: permissions, // Send full list so device can work offline
                timestamp: Date.now()
            });
        }
        // -----------------------------------

        res.json({ success: true, device, role: role.name, permissions });
    } catch (error) {
        console.error('Error assigning role:', error);
        res.status(500).json({ error: error.message });
    }
});

// Push Dynamic UI Layout to Device
router.post('/api/devices/:id/layout', async (req, res) => {
    try {
        const deviceId = req.params.id;
        const { layout } = req.body;

        if (!layout) return res.status(400).json({ error: 'Layout JSON required' });

        // --- REAL-TIME PUSH: UI LAYOUT ---
        if (global.sendToDevice) {
            const sent = global.sendToDevice(deviceId, 'LAYOUT_UPDATE', {
                layout: layout,
                timestamp: Date.now()
            });

            if (sent) {
                return res.json({ success: true, message: 'Layout pushed to device' });
            } else {
                return res.status(404).json({ error: 'Device not connected via WebSocket' });
            }
        }
        // -----------------------------------

        res.status(503).json({ error: 'WebSocket service unavailable' });
    } catch (error) {
        console.error('Error pushing layout:', error);
        res.status(500).json({ error: error.message });
    }
});

// Get all items with issues
router.get('/items/issues', (req, res) => {
    const itemsWithIssues = [];
    
    global.items.forEach((item, key) => {
        if (item.actn && item.actn.some(action => ['check', 'cause', 'result'].includes(action[0]))) {
            itemsWithIssues.push({
                id: key,
                serialNumber: key.startsWith('i7') ? key.slice(-7) : key,
                model: item.cl || 'Unknown',
                actions: item.actn || [],
                location: item.loc && item.loc.length > 0 ? item.loc[item.loc.length - 1] : null
            });
        }
    });
    
    res.json(itemsWithIssues);
});

// Get all boxes currently in processing
router.get('/boxes/processing', (req, res) => {
    const processingBoxes = [];
    
    global.boxes.forEach((box, key) => {
        let packIn = false;
        let packOut = false;
        
        box.loc?.forEach((locElement) => {
            if (locElement[0] === 'p000000000000000030') packIn = locElement[1];
            if (locElement[0] === 'p000000000000000060') packOut = locElement[1];
        });
        
        // Box is in processing if it has been packed in but not packed out
        if (packIn && !packOut) {
            processingBoxes.push({
                id: key,
                serialNumber: key.replace(/^b0+/, ''),
                inDate: packIn,
                contents: box.cont || [],
                linkedOrders: box.in?.filter(link => link[0].startsWith('o')) || []
            });
        }
    });
    
    res.json(processingBoxes);
});

// Generate new labels/codes
router.post('/generate-codes', async (req, res) => {
    try {
        let { type, startNumber, count, dimensions, cols, rows } = req.body;

        if (!['i', 'b', 'p', 'l', 'marker'].includes(type)) {
            return res.status(400).json({ error: 'Invalid code type. Must be i, b, p, l, or marker.' });
        }

        // Validate count
        if (!count || isNaN(count)) {
            return res.status(400).json({ error: 'Count must be a valid number.' });
        }
        count = parseInt(count);

        // Parse layout dimensions (default to 2x16 if not provided)
        const layoutCols = cols ? parseInt(cols) : 2;
        const layoutRows = rows ? parseInt(rows) : 16;

        // Import SystemSetting model
        const { SystemSetting } = require('../../../shared/models/postgresql');

        // Map type to database counter key (marker is legacy alias for 'l')
        const counterKeyMap = {
            'i': 'last_serial_item',
            'b': 'last_serial_box',
            'p': 'last_serial_place',
            'l': 'last_serial_marker',
            'marker': 'last_serial_marker'
        };

        const counterKey = counterKeyMap[type];

        // If startNumber not provided, fetch the next available number from DB
        if (!startNumber || startNumber === '' || isNaN(startNumber)) {
            const lastSerial = await SystemSetting.getValue(counterKey, '0');
            startNumber = parseInt(lastSerial) + 1;
        } else {
            startNumber = parseInt(startNumber);
        }

        // Normalize 'marker' to 'l' for PDF generation (backward compatibility)
        const pdfType = type === 'marker' ? 'l' : type;
        const filename = `eckwms_${pdfType}${startNumber}.pdf`;
        const filePath = path.join(global.baseDirectory, filename);

        // Generate PDF file with betruger encoding for all types
        // Pass layout dimensions to support different grid layouts
        betrugerPrintCodesPdf(pdfType, startNumber, dimensions || [['', 5], ['', 20]], count, layoutCols, layoutRows);

        // Update the database counter
        const newLastSerial = startNumber + count - 1;
        await SystemSetting.setValue(counterKey, newLastSerial.toString());

        // Also update global variables for backward compatibility (optional)
        if (type === 'i') {
            global.serialI = Math.max(global.serialI || 0, newLastSerial);
        } else if (type === 'b') {
            global.serialB = Math.max(global.serialB || 0, newLastSerial);
        } else if (type === 'p') {
            global.serialP = Math.max(global.serialP || 0, newLastSerial);
        }

        // Send PDF file for download
        res.download(filePath, filename, (err) => {
            if (err) {
                console.error("Error sending file:", err);
                return res.status(500).json({ error: 'Error sending file' });
            }
        });
    } catch (error) {
        console.error("Error generating codes:", error);
        return res.status(500).json({ error: error.message || 'Error generating codes' });
    }
});

// Create new item
router.post('/items', (req, res) => {
    const { classCode, attributes } = req.body;
    
    if (!classCode) {
        return res.status(400).json({ error: 'Class code is required' });
    }
    
    const itemSN = 'i' + (('000000000000000000' + (++global.serialI)).slice(-18));
    const newItem = Object.create(global.item);
    
    newItem.sn = [itemSN, Math.floor(Date.now() / 1000)];
    newItem.cl = classCode;
    
    // Add attributes if provided
    if (attributes) {
        newItem.attr = attributes;
    }
    
    // Set prototype from class if available
    if (global.classes.has(classCode)) {
        Object.setPrototypeOf(newItem, global.classes.get(classCode));
    }
    
    // Add to items collection
    global.items.set(itemSN, newItem);
    
    // Add to class's down property
    if (global.classes.has(classCode)) {
        addUnicEntryToProperty(global.classes, classCode, ['i', itemSN], 'down');
    }

    // Sync public data to global server
    syncPublicData({ id: itemSN, type: 'item', data: { classCode: newItem.cl, createdAt: newItem.sn[1] } });

    res.status(201).json({
        id: itemSN,
        serialNumber: itemSN.startsWith('i7') ? itemSN.slice(-7) : itemSN,
        createdAt: newItem.sn[1]
    });
});

// Create new box
router.post('/boxes', (req, res) => {
    const boxSN = 'b' + (('000000000000000000' + (++global.serialB)).slice(-18));
    const newBox = Object.create(global.box);
    
    newBox.sn = [boxSN, Math.floor(Date.now() / 1000)];
    newBox.cont = [];
    newBox.loc = [];
    
    // Add to boxes collection
    global.boxes.set(boxSN, newBox);

    // Sync public data to global server
    syncPublicData({ id: boxSN, type: 'box', data: { createdAt: newBox.sn[1] } });

    res.status(201).json({
        id: boxSN,
        serialNumber: boxSN.replace(/^b0+/, ''),
        createdAt: newBox.sn[1]
    });
});

// Add item to box
router.post('/boxes/:boxId/items/:itemId', (req, res) => {
    const boxId = req.params.boxId;
    const itemId = req.params.itemId;
    
    // Check if both box and item exist
    if (!global.boxes.has(boxId)) {
        return res.status(404).json({ error: 'Box not found' });
    }
    
    if (!global.items.has(itemId)) {
        return res.status(404).json({ error: 'Item not found' });
    }
    
    const timestamp = Math.floor(Date.now() / 1000);
    
    // Add item to box
    addEntryToProperty(global.boxes, boxId, [itemId, timestamp], 'cont');
    
    // Update item location
    addEntryToProperty(global.items, itemId, [boxId, timestamp], 'loc');
    
    res.status(200).json({ 
        success: true, 
        message: 'Item added to box',
        timestamp
    });
});

// Move box to place
router.post('/boxes/:boxId/place/:placeId', (req, res) => {
    const boxId = req.params.boxId;
    const placeId = req.params.placeId;
    
    // Check if both box and place exist
    if (!global.boxes.has(boxId)) {
        return res.status(404).json({ error: 'Box not found' });
    }
    
    if (!global.places.has(placeId)) {
        return res.status(404).json({ error: 'Place not found' });
    }
    
    const timestamp = Math.floor(Date.now() / 1000);
    
    // Add place to box's location
    addEntryToProperty(global.boxes, boxId, [placeId, timestamp], 'loc');
    
    // Add box to place's contents
    addEntryToProperty(global.places, placeId, [boxId, timestamp], 'cont');
    
    res.status(200).json({ 
        success: true, 
        message: 'Box moved to place',
        timestamp
    });
});

// Add action to item
router.post('/items/:itemId/actions', (req, res) => {
    const itemId = req.params.itemId;
    const { type, message } = req.body;
    
    if (!global.items.has(itemId)) {
        return res.status(404).json({ error: 'Item not found' });
    }
    
    if (!type || !message) {
        return res.status(400).json({ error: 'Action type and message are required' });
    }
    
    const timestamp = Math.floor(Date.now() / 1000);
    
    // Add action to item
    addEntryToProperty(global.items, itemId, [type, message, timestamp], 'actn');
    
    res.status(200).json({ 
        success: true, 
        message: 'Action added to item',
        timestamp
    });
});

// Get all pending RMAs
router.get('/rmas/pending', (req, res) => {
    const pendingRMAs = [];
    
    global.orders.forEach((order, key) => {
        // Filter orders with RMA code but no contents yet
        if (key.startsWith('o') && key.includes('RMA') && (!order.cont || order.cont.length === 0)) {
            pendingRMAs.push({
                id: key,
                rmaCode: key.slice(4), // Remove 'o000' prefix
                createdAt: order.sn[1],
                company: order.comp,
                person: order.pers,
                email: order.cem,
                declarations: order.decl || []
            });
        }
    });
    
    res.json(pendingRMAs);
});

// Export CSV with service data
router.get('/export/csv', (req, res) => {
    let csv = 'SN /PN;Model;IN DATE;Out Date;Customer;SKU;email;Address;Zip Code;City;Complaint;Verification;Cause;Result;Shipping;Invoice number;Special note; warranty;condition;Used New Parts;Used Refurbished Parts\n';
    
    global.boxes.forEach((element) => {
        let packIn = false;
        let packOut = false;
        element.loc?.forEach((locElement) => {
            if (locElement[0] == 'p000000000000000030') packIn = locElement[1];
            if (locElement[0] == 'p000000000000000060') packOut = locElement[1];
        });

        if (packIn || packOut) {
            let shippingCode = '';
            let orderCode = '';
            let customerName = '';
            let Bemail = '';
            let BAddress = '';
            let BZipCode = '';
            let BCity = '';
            let hasOrder = [];
            
            // Extract shipping info
            element.brc?.forEach((brcElement, index) => {
                // Shipping code extraction logic (UPS, DPD, etc.)
                const upsRegex = /^1Z[0-9A-Z]{16}$/;
                const dpdRegex = /^%[0-9a-zA-Z]{7}(\d{14})\d{6}$/;
                const fedExRegex = /^\d{24}$|^\d{34}$/;
                const dhlExpressRegex = /^JJD\d{9,10}$/;
                const dhlPaketRegex = /^\d{12,14}$/;
                const dhlGlobalMailRegex = /^GM\d{9}DE$/;
                
                if (upsRegex.test(brcElement)) shippingCode += `${brcElement}_UPS `;
                else if (match = brcElement.match(dpdRegex)) shippingCode += `${match[1]}_DPD `;
                else if (fedExRegex.test(brcElement)) shippingCode += `${brcElement}_FedEx `;
                else if (dhlExpressRegex.test(brcElement)) shippingCode += `${brcElement}_DHLexp `;
                else if (dhlGlobalMailRegex.test(brcElement)) shippingCode += `${brcElement}_DHLmail `;
            });
            
            // Find order and customer info
            if (element.in?.length) {
                element.in.forEach((link) => {
                    if (link[0].startsWith('o')) {
                        orderCode = link[0].slice(2).replace(/^0*/, '');
                        hasOrder.push(link[0]);
                        if (global.orders.has(link[0])) {
                            customerName = global.orders.get(link[0])?.comp;
                            Bemail = global.orders.get(link[0])?.cem;
                            BAddress = global.orders.get(link[0])?.str + ' ' + global.orders.get(link[0])?.hs;
                            BZipCode = global.orders.get(link[0])?.zip;
                            BCity = global.orders.get(link[0])?.cit;
                        }
                    } else if (link[0].startsWith('u')) {
                        if (global.users.has(link[0])) {
                            customerName = global.users.get(link[0])?.comp;
                            Bemail = global.users.get(link[0])?.cem;
                            BAddress = global.users.get(link[0])?.str + ' ' + link[0]?.hs;
                            BZipCode = global.users.get(link[0])?.zip;
                            BCity = global.users.get(link[0])?.cit;
                        }
                    } else {
                        customerName = link[0];
                    }
                });
            }
            
            // Process each device in the box
            const uniqueDevices = new Map();
            
            element.cont?.forEach(dev => {
                const serialNumber = dev[0];
                const timestamp = dev[1];
                
                if (!uniqueDevices.has(serialNumber) || uniqueDevices.get(serialNumber).timestamp < timestamp) {
                    uniqueDevices.set(serialNumber, { timestamp, dev });
                }
            });
            
            // Generate CSV rows for each device
            uniqueDevices.forEach(({ dev }) => {
                if (!global.items.has(dev[0])) return;
                
                const item = global.items.get(dev[0]);
                let mcheck = '';
                let mcause = '';
                let mresult = '';
                let mnote = '';
                
                item.actn?.forEach(([type, message, timestamp]) => {
                    if (type == 'check') mcheck += message;
                    if (type == 'cause') mcause += message;
                    if (type == 'result') mresult += message;
                    if (type == 'note') mnote += message;
                });
                
                if (orderCode.includes('RMA')) {
                    csv += `${dev[0].slice(2).replace(/^0*/, '')};${item?.attr?.MN ?? ''};${packIn ? formatUnixTimestamp(packIn).slice(0, 10) : ''};` +
                        `${packOut ? formatUnixTimestamp(packOut).slice(0, 10) : ''};` +
                        `${customerName};${item?.cl ?? ''};${Bemail};${BAddress};${BZipCode};${BCity};   ;${mcheck};${mcause};${mresult};${shippingCode};${orderCode};` +
                        `${mnote};   ;   ;   ;    ;   \n`;
                }
            });
        }
    });
    
    res.writeHead(200, {
        'Content-Type': 'text/csv',
        'Content-Disposition': 'attachment; filename="service_data.csv"',
        'Content-Length': csv.length
    });
    
    res.end(csv);
});

module.exports = router;