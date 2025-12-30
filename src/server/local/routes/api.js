// routes/api.js
const express = require('express');
const router = express.Router();
const { verifyJWT } = require('../../../shared/utils/encryption');
const { prettyPrintObject, maskObjectFields } = require('../utils/formatUtils');
const { findKnownCode, isBetDirect } = require('../utils/dataInit');
const OpenAI = require("openai");
const { processDocument } = require('../services/documentService');
const inventoryService = require('../services/inventoryService');

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
    const code = req.params.code;
    let betCode = findKnownCode(code) || isBetDirect(code);

    if (!betCode) {
        return res.status(404).json({ error: 'Invalid item code' });
    }

    const item = await inventoryService.get('item', betCode);
    if (!item) {
        return res.status(404).json({ error: 'Item not found' });
    }

    res.json(item);
});

// Get item actions
router.get('/item/:code/actions', (req, res) => {
    const code = req.params.code;
    let betCode = findKnownCode(code) || isBetDirect(code);
    
    if (!betCode) {
        return res.status(404).json({ error: 'Invalid item code' });
    }

    const item = global.items.get(betCode);
    if (!item) {
        return res.status(404).json({ error: 'Item not found' });
    }

    res.json(item.actn || []);
});

// Get box information
router.get('/box/:code', async (req, res) => {
    const code = req.params.code;
    let betCode = isBetDirect(code);

    if (!betCode || betCode[0] !== 'b') {
        return res.status(404).json({ error: 'Invalid box code' });
    }

    const box = await inventoryService.get('box', betCode);
    if (!box) {
        return res.status(404).json({ error: 'Box not found' });
    }

    res.json(box);
});

// Get order information (protected)
router.get('/order/:code', authenticateJWT, (req, res) => {
    const code = req.params.code;
    let betCode = isBetDirect(code);
    
    if (!betCode || betCode[0] !== 'o') {
        return res.status(404).json({ error: 'Invalid order code' });
    }

    const order = global.orders.get(betCode);
    if (!order) {
        return res.status(404).json({ error: 'Order not found' });
    }

    // Mask sensitive fields for regular users, but not for admin
    const maskedOrder = req.user.a === 'p' ? order : maskObjectFields(order, ["comp", "pers", "str", "cem", "iem"]);
    
    res.json(maskedOrder);
});

// Get item location history
router.get('/item/:code/location', (req, res) => {
    const code = req.params.code;
    let betCode = findKnownCode(code) || isBetDirect(code);
    
    if (!betCode) {
        return res.status(404).json({ error: 'Invalid item code' });
    }

    const item = global.items.get(betCode);
    if (!item) {
        return res.status(404).json({ error: 'Item not found' });
    }

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