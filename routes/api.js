// routes/api.js
const express = require('express');
const router = express.Router();
const { verifyJWT } = require('../utils/encryption');
const { prettyPrintObject, maskObjectFields } = require('../utils/formatUtils');
const { findKnownCode, isBetDirect } = require('../utils/dataInit');
const OpenAI = require("openai");

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
router.get('/item/:code', (req, res) => {
    const code = req.params.code;
    let betCode = findKnownCode(code) || isBetDirect(code);
    
    if (!betCode) {
        return res.status(404).json({ error: 'Invalid item code' });
    }

    const item = global.items.get(betCode);
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
router.get('/box/:code', (req, res) => {
    const code = req.params.code;
    let betCode = isBetDirect(code);
    
    if (!betCode || betCode[0] !== 'b') {
        return res.status(404).json({ error: 'Invalid box code' });
    }

    const box = global.boxes.get(betCode);
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

// Translate text using OpenAI
router.post('/translate', async (req, res) => {
    try {
        const { text, targetLang } = req.body;
        
        if (!text || !targetLang) {
            return res.status(400).json({ error: 'Text and target language required' });
        }

        const translatedText = await translateText(text, targetLang);
        res.json({ original: text, translated: translatedText, language: targetLang });
    } catch (error) {
        console.error("Translation error:", error);
        res.status(500).json({ error: "Translation failed" });
    }
});



// Helper function to translate text
async function translateText(text, targetLang) {
    try {
        // Create a chat completion
        const completion = await openai.chat.completions.create({
            model: "gpt-4o-mini",
            store: true, // Store data if needed
            messages: [
                { role: "system", content: `You are a professional translator. Translate this to ${targetLang}.` },
                { role: "user", content: text }
            ]
        });

        // Extract the translated text from the response
        return completion.choices[0].message.content.trim();
    } catch (error) {
        console.error("Translation error:", error);
        throw new Error("Failed to translate text.");
    }
}

module.exports = router;