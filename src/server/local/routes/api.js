// routes/api.js
const express = require('express');
const router = express.Router();
const { verifyJWT } = require('../../../shared/utils/encryption');
const { prettyPrintObject, maskObjectFields } = require('../utils/formatUtils');
const OpenAI = require("openai");
const { processDocument } = require('../services/documentService');
const inventoryService = require('../services/inventoryService');

// Simple code identification (Clean Slate - no legacy lookup)
function identifyCode(code) {
    if (code.startsWith('i')) return code;
    if (code.startsWith('b')) return code;
    if (code.startsWith('p')) return code;
    if (code.startsWith('o')) return code;
    return null;
}

// Initialize OpenAI
const openai = new OpenAI({
    apiKey: process.env.OPENAI_API_KEY,
});

// Middleware to verify JWT for protected routes
const authenticateJWT = (req, res, next) => {
    try {
        const token = req.headers.authorization?.split(' ')[1];
        if (!token) {
            return res.status(401).json({ error: 'Authentication token required' });
        }
        
        req.user = verifyJWT(token, global.secretJwt);
        next();
    } catch (error) {
        return res.status(403).json({ error: 'Invalid or expired token' });
    }
};

// Get item information by code
router.get('/item/:code', async (req, res) => {
    const code = identifyCode(req.params.code);
    if (!code) return res.status(404).json({ error: 'Invalid item code' });

    const item = await inventoryService.get('item', code);
    if (!item) return res.status(404).json({ error: 'Item not found' });

    res.json(item);
});

// Get item actions
router.get('/item/:code/actions', async (req, res) => {
    const code = identifyCode(req.params.code);
    if (!code) return res.status(404).json({ error: 'Invalid item code' });

    const item = await inventoryService.get('item', code);
    if (!item) return res.status(404).json({ error: 'Item not found' });

    res.json(item.actn || []);
});

// Get box information
router.get('/box/:code', async (req, res) => {
    const code = identifyCode(req.params.code);
    if (!code || !code.startsWith('b')) return res.status(404).json({ error: 'Invalid box code' });

    const box = await inventoryService.get('box', code);
    if (!box) return res.status(404).json({ error: 'Box not found' });

    res.json(box);
});

// Get order information (protected)
router.get('/order/:code', authenticateJWT, async (req, res) => {
    const code = identifyCode(req.params.code);
    if (!code || !code.startsWith('o')) return res.status(404).json({ error: 'Invalid order code' });

    // Orders migration TBD - for now return placeholder
    return res.status(501).json({ error: 'Order lookup not yet implemented in clean slate' });
});

// Get item location history
router.get('/item/:code/location', async (req, res) => {
    const code = identifyCode(req.params.code);
    if (!code) return res.status(404).json({ error: 'Invalid item code' });

    const item = await inventoryService.get('item', code);
    if (!item) return res.status(404).json({ error: 'Item not found' });

    res.json(item.loc || []);
});

// Generic endpoint for submitting documents from workflows
router.post('/documents', authenticateJWT, async (req, res) => {
  const { type, payload, format } = req.body;

  if (!type || !payload) {
    return res.status(400).json({ error: 'Document type and payload are required.' });
  }

  try {
    const result = await processDocument(type, payload);
    res.status(201).json({
      success: true,
      message: `Document '${type}' processed successfully.`,
      result
    });
  } catch (error) {
    console.error(`[API /documents] Error processing document type '${type}':`, error);
    res.status(500).json({ success: false, error: error.message });
  }
});

module.exports = router;