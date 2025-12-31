const express = require('express');
const router = express.Router();
const { formatUnixTimestamp } = require('../utils/formatUtils');
const inventoryService = require('../services/inventoryService');

router.get('/serial/:code', async (req, res) => {
    const code = req.params.code;

    // 1. Device Serial Check (7 digits)
    if (/^\d{7}$/.test(code)) {
        const deviceKey = 'i70000000000' + code;
        const device = await inventoryService.get('item', deviceKey);

        if (!device) return res.send(`<br><em>Device not found.</em>`);

        let html = `Device SN: <strong>${code}</strong><br>Registered on: ${formatUnixTimestamp(device.sn[1])}<br><br>`;

        if (device.actn && device.actn.length > 0) {
            html += `Actions:<ul>`;
            device.actn.forEach(action => {
                const [type, text, ts] = action;
                if (type !== 'note') {
                    const style = (type === 'result' && ts > device.sn[1]) ? 'style="color: green;"' : '';
                    html += `<li ${style}>${type.charAt(0).toUpperCase() + type.slice(1)}: ${text} (${formatUnixTimestamp(ts)})</li>`;
                }
            });
            html += `</ul><br><br>`;
        }

        // Check location
        if (device.loc && device.loc.length > 0) {
            const lastLoc = device.loc[device.loc.length - 1];
            html += `Current Location: ${lastLoc[0]} (${formatUnixTimestamp(lastLoc[1])})<br>`;
             if (lastLoc[0] === 'p000000000000000060') {
                html += `<br><em style="color: green; font-weight: bold;">Device has been sent back to the client.</em>`;
            }
        } else {
            html += `<em>No location info.</em><br>`;
        }
        return res.send(html);
    }

    // 2. RMA Check (Legacy + New)
    if (/^RMA\d{10}[A-Za-z0-9]{2}$/.test(code)) {
         // Try finding in DB via RmaRequest model directly for better performance or via legacy mapping
         // For consistency with migration, we assume Order objects are migrated to 'orders' table or similar,
         // BUT current migration strategy put Items/Boxes/Places.
         // Orders are still in global.orders in server.js?
         // CRITICAL: We haven't migrated Orders yet. Fallback to global.orders for now, or check RmaRequest PG model.

         // Using RmaRequest PG model as primary source
         const { RmaRequest } = require('../../../shared/models/postgresql');
         const rma = await RmaRequest.findOne({ where: { rmaCode: code } });

         if (rma) {
             let html = `RMA <b>${code}</b> found.<br>Status: ${rma.status}<br>Created: ${new Date(rma.createdAt).toLocaleString()}<br><br>`;
             // Add tracking logic here if needed
             return res.send(html);
         }

         return res.send(`<br><em>RMA not found.</em>`);
    }

    return res.status(400).send("Invalid format");
});

module.exports = router;
