// routes/admin.js
const express = require('express');
const router = express.Router();
const { eckPrintCodesPdf } = require('../utils/pdfGeneratorNew');
const { requireAdmin } = require('../middleware/auth');
const path = require('path');
const db = require('../../../shared/models/postgresql');
const inventoryService = require('../services/inventoryService');

// HTML Routes
router.get('/', (req, res) => res.sendFile(path.join(__dirname, '../views/admin/index.html')));
router.get('/pairing', (req, res) => res.sendFile(path.join(__dirname, '../views/admin/pairing.html')));
router.get('/printing', (req, res) => res.sendFile(path.join(__dirname, '../views/admin/printing.html')));
router.get('/blueprint', (req, res) => res.sendFile(path.join(__dirname, '../views/admin/blueprint.html')));

// API Routes
router.use('/api', requireAdmin);

// Devices
router.get('/api/devices', async (req, res) => {
    try {
        const devices = await db.RegisteredDevice.findAll({ order: [['status', 'DESC'], ['updatedAt', 'DESC']] });
        res.json(devices);
    } catch (e) { res.status(500).json({ error: e.message }); }
});

router.post('/api/devices/:id/status', async (req, res) => {
    try {
        const device = await db.RegisteredDevice.findByPk(req.params.id);
        if (!device) return res.status(404).json({ error: 'Device not found' });
        device.status = req.body.status;
        await device.save();
        if (global.sendToDevice) global.sendToDevice(device.deviceId, 'STATUS_UPDATE', { status: device.status, active: device.status === 'active' });
        res.json({ success: true, device });
    } catch (e) { res.status(500).json({ error: e.message }); }
});

router.delete('/api/devices/:id', async (req, res) => {
    try {
        await db.RegisteredDevice.destroy({ where: { deviceId: req.params.id } });
        res.json({ success: true });
    } catch (e) { res.status(500).json({ error: e.message }); }
});

// Inventory Issues
router.get('/items/issues', async (req, res) => {
    try {
        const [results] = await db.sequelize.query(`
            SELECT id, data FROM items WHERE EXISTS (
                SELECT 1 FROM jsonb_array_elements(data->'actn') as action WHERE action->>0 IN ('check', 'cause', 'result')
            )
        `);
        res.json(results.map(row => ({
            id: row.id,
            serialNumber: row.id.startsWith('i7') ? row.id.slice(-7) : row.id,
            model: row.data.cl || 'Unknown',
            actions: row.data.actn || []
        })));
    } catch (e) { res.status(500).json({ error: e.message }); }
});

// Processing Boxes
router.get('/boxes/processing', async (req, res) => {
    try {
        const boxes = await inventoryService.getAll('box');
        const processingBoxes = boxes.filter(box => {
            // Logic: Has IN location (30) but no OUT location (60)
            // Note: This is legacy logic logic ported to new data structure
            let packIn = false, packOut = false;
            box.loc?.forEach(l => {
                if (l[0] === 'p000000000000000030') packIn = true;
                if (l[0] === 'p000000000000000060') packOut = true;
            });
            return packIn && !packOut;
        });
        res.json(processingBoxes);
    } catch (e) { res.status(500).json({ error: e.message }); }
});

// Next Serials
router.get('/api/next-serials', async (req, res) => {
    try {
        const serials = {
            i: await db.SystemSetting.getValue('last_serial_item', '0'),
            b: await db.SystemSetting.getValue('last_serial_box', '0'),
            p: await db.SystemSetting.getValue('last_serial_place', '0'),
            l: await db.SystemSetting.getValue('last_serial_marker', '0')
        };
        res.json({ i: parseInt(serials.i) + 1, b: parseInt(serials.b) + 1, p: parseInt(serials.p) + 1, l: parseInt(serials.l) + 1 });
    } catch (e) { res.status(500).json({ error: e.message }); }
});

// RMA List
router.get('/rmas/pending', async (req, res) => {
    try {
        const rmas = await db.RmaRequest.findAll({
            where: { status: 'created' },
            order: [['createdAt', 'DESC']]
        });
        res.json(rmas);
    } catch (e) { res.status(500).json({ error: e.message }); }
});

// Simple CSV Export (Rebuilt for DB)
router.get('/export/csv', async (req, res) => {
    try {
        // Fetch boxes that are in processing or shipped
        // For now, dumping all boxes with basic info as proof of concept for clean slate
        const boxes = await db.Box.findAll();
        let csv = 'Box ID;Created At;Contents Count\n';

        boxes.forEach(box => {
            const data = box.data || {};
            csv += `${box.id};${box.createdAt};${data.cont ? data.cont.length : 0}\n`;
        });

        res.writeHead(200, {
            'Content-Type': 'text/csv',
            'Content-Disposition': 'attachment; filename="service_data.csv"'
        });
        res.end(csv);
    } catch (e) { res.status(500).send('Error: ' + e.message); }
});

// Label Generation
router.post('/generate-codes', async (req, res) => {
    try {
        const { type, startNumber, count, serialDigits, layoutParams, contentConfig, warehouseConfig } = req.body;

        let actualStart = startNumber;
        const counterKey = type === 'i' ? 'last_serial_item' : type === 'b' ? 'last_serial_box' : type === 'p' ? 'last_serial_place' : 'last_serial_marker';

        if (!actualStart) {
            const last = await db.SystemSetting.getValue(counterKey, '0');
            actualStart = parseInt(last) + 1;
        }

        const pdfBuffer = await eckPrintCodesPdf(type === 'marker' ? 'l' : type, parseInt(actualStart), {
            ...layoutParams,
            serialDigits: parseInt(serialDigits) || 0,
            contentConfig,
            warehouseConfig
        }, parseInt(count));

        // Update counter if using auto-increment
        if (!startNumber) {
             await db.SystemSetting.setValue(counterKey, (parseInt(actualStart) + parseInt(count) - 1).toString());
        }

        res.set({
            'Content-Type': 'application/pdf',
            'Content-Disposition': `attachment; filename="labels_${type}.pdf"`
        });
        res.end(pdfBuffer);
    } catch (e) {
        console.error(e);
        res.status(500).json({ error: e.message });
    }
});

// Warehouse API
router.get('/api/warehouses', async (req, res) => {
    try {
        const warehouses = await db.Warehouse.findAll({ order: [['id', 'ASC']] });
        res.json(warehouses);
    } catch (e) { res.status(500).json({ error: e.message }); }
});

// Warehouse Racks API
router.get('/api/warehouse/racks', async (req, res) => {
    try {
        const warehouseId = req.query.warehouseId;
        const where = warehouseId ? { warehouse_id: warehouseId } : {};
        const racks = await db.WarehouseRack.findAll({
            where,
            order: [['sort_order', 'ASC'], ['id', 'ASC']]
        });
        res.json(racks);
    } catch (e) { res.status(500).json({ error: e.message }); }
});

router.post('/api/warehouse/racks', async (req, res) => {
    try {
        // Upsert logic: update if ID exists, create if not
        if (req.body.id) {
            const existing = await db.WarehouseRack.findByPk(req.body.id);
            if (existing) {
                await existing.update(req.body);
                return res.json(existing);
            }
        }
        const rack = await db.WarehouseRack.create(req.body);
        res.json(rack);
    } catch (e) { res.status(500).json({ error: e.message }); }
});

router.put('/api/warehouse/racks/:id', async (req, res) => {
    try {
        const rack = await db.WarehouseRack.findByPk(req.params.id);
        if (!rack) return res.status(404).json({ error: 'Rack not found' });
        await rack.update(req.body);
        res.json(rack);
    } catch (e) { res.status(500).json({ error: e.message }); }
});

router.delete('/api/warehouse/racks/:id', async (req, res) => {
    try {
        await db.WarehouseRack.destroy({ where: { id: req.params.id } });
        res.json({ success: true });
    } catch (e) { res.status(500).json({ error: e.message }); }
});

module.exports = router;
