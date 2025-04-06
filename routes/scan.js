// routes/scan.js
const express = require('express');
const router = express.Router();
const { processScan } = require('../utils/scanHandler');

/**
 * Process a scanned barcode
 * POST /api/scan/process
 */
router.post('/process', async (req, res) => {
    try {
        const { barcode, type } = req.body;
        
        if (!barcode) {
            return res.status(400).json({ 
                success: false, 
                text: "Barcode is required"
            });
        }
        
        console.log(`Processing scan - Barcode: ${barcode}, Format Type: ${type}`);
        
        // Process the scan using our existing handler
        const result = await processScan(barcode, req.user);
        
        // Create response in the new format with text field
        const response = {
            success: true,
            text: result.message || "Scan processed successfully",
            barcodeType: type || "UNKNOWN",
            contentType: result.type || "unknown",
            ...result.data,
            buffers: result.buffers
        };
        
        res.json(response);
    } catch (error) {
        console.error("Error processing scan:", error);
        res.status(500).json({
            success: false,
            text: error.message || "Error processing scan"
        });
    }
});

/**
 * Get recent scans
 * GET /api/scan/recent
 */
router.get('/recent', async (req, res) => {
    try {
        // Get recent scans (implement this functionality)
        // For now, return empty array
        res.json({
            success: true,
            text: "Recent scans retrieved",
            scans: [] // Array of recent scans
        });
    } catch (error) {
        console.error("Error retrieving recent scans:", error);
        res.status(500).json({
            success: false,
            text: error.message || "Error retrieving recent scans"
        });
    }
});

module.exports = router;