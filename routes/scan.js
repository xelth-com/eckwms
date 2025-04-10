// routes/scan.js
const express = require('express');
const router = express.Router();
const { processScan } = require('../utils/scanHandler');

// Endpoint for processing scanned barcodes
router.post('/process', async (req, res) => {
    try {
        const { barcode, type, deviceId } = req.body;
        
        if (!barcode) {
            return res.status(400).json({
                success: false,
                text: 'Barcode is required'
            });
        }

        console.log(`Processing scan request for barcode: ${barcode}, type: ${type}, device: ${deviceId}`);
        
        // Process the scan using the scanHandler utility
        const result = await processScan(barcode);
        
        // Add sample images for testing - in a real implementation, these would be dynamically
        // generated based on the scanned item
        // Format the response for the mobile app
        const response = {
            success: true,
            contentType: result.type,
            text: result.message,
            data: result.data,
            buffers: result.buffers,
            device: deviceId,
            type: type || 'unknown'
        };
        
        // Add actual images from your server storage
        // Use real images from the server for any barcode type
        response.images = [
            'https://pda.repair/storage/pics/BK10.webp',
            'https://pda.repair/storage/pics/SL20.webp',
            'https://pda.repair/storage/pics/SM20.webp'
        ];
        
        // Optionally add specific images based on barcode type
        if (barcode.length === 7 && /^\d+$/.test(barcode)) {
            // For 7-digit codes, add additional images
            response.images.push('https://pda.repair/storage/pics/OX10.webp');
            response.images.push('https://pda.repair/storage/pics/US20.webp');
        } else if (barcode.startsWith('b')) {
            // For box barcodes
            response.images.push('https://pda.repair/storage/pics/sm15.webp');
            response.images.push('https://pda.repair/storage/pics/ul20.webp');
        } else if (barcode.startsWith('RMA')) {
            // For RMA codes
            response.images.push('https://pda.repair/storage/pics/frankfurt.avif');
            response.images.push('https://pda.repair/storage/pics/SEN_8281.avif');
        }

        res.json(response);
    } catch (error) {
        console.error('Error processing scan:', error);
        res.status(500).json({
            success: false,
            text: `Server error: ${error.message}`
        });
    }
});

// Sample endpoint for serving images
router.get('/images/:type/:filename', (req, res) => {
    const { type, filename } = req.params;
    
    // In a real implementation, this would fetch actual images.
    // For demo, we'll generate a placeholder image with text
    
    // Return a 404 for 10% of requests to test error handling
    if (Math.random() < 0.1) {
        return res.status(404).send('Image not found');
    }
    
    // Set content type for image response
    res.setHeader('Content-Type', 'image/jpeg');
    
    // This is a placeholder. In a real implementation, 
    // you would serve actual image files from storage
    res.send(`This would be a ${type} image for ${filename}`);
});

module.exports = router;