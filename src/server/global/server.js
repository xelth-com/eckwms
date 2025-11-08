// eckWMS Global Server
// Public-facing server that proxies API requests and serves public information pages

require('dotenv').config({ path: require('path').resolve(__dirname, '../../../.env') });
const express = require('express');
const path = require('path');
const { createProxyMiddleware } = require('http-proxy-middleware');
const { betrugerUrlDecrypt } = require('../../shared/utils/encryption');

const app = express();
const PORT = process.env.GLOBAL_SERVER_PORT || 8080;
const LOCAL_SERVER_URL = process.env.LOCAL_SERVER_INTERNAL_URL || 'http://localhost:3000';

// Setup view engine for public pages
app.set('view engine', 'ejs');
app.set('views', path.join(__dirname, 'views'));

// Middleware for parsing JSON
app.use(express.json());
app.use(express.urlencoded({ extended: true }));

// Logging middleware
app.use((req, res, next) => {
    console.log(`[Global Server] ${req.method} ${req.url}`);
    next();
});

// API Proxy Middleware
// Forwards requests from mobile clients to the internal local server
app.use('/api/proxy', createProxyMiddleware({
    target: LOCAL_SERVER_URL,
    changeOrigin: true,
    pathRewrite: { '^/api/proxy': '' }, // Remove /api/proxy prefix before forwarding
    logLevel: process.env.NODE_ENV === 'development' ? 'debug' : 'info',
    onError: (err, req, res) => {
        console.error('[Global Server] Proxy error:', err.message);
        if (res && !res.headersSent) {
            res.status(502).json({
                error: 'Proxy Error',
                message: 'Could not connect to local server.'
            });
        }
    }
}));

// Health check endpoint
app.get('/health', (req, res) => {
    res.json({
        status: 'ok',
        server: 'global',
        timestamp: new Date().toISOString()
    });
});

// Public Code Route
// Displays public information for scanned QR codes
app.get('/:code', async (req, res) => {
    const { code } = req.params;

    // Skip common browser requests
    if (code === 'favicon.ico') {
        return res.status(404).end();
    }

    console.log(`[Global Server] Processing code: ${code}`);

    // Decrypt the code to get the internal ID
    const decryptedId = betrugerUrlDecrypt(`ECK1.COM/${code}M3`);

    if (!decryptedId) {
        console.log(`[Global Server] Invalid or expired code: ${code}`);
        return res.status(404).render('public-view', {
            data: { error: 'Code not valid or expired.' }
        });
    }

    console.log(`[Global Server] Decrypted ID: ${decryptedId}`);

    try {
        // Fetch public data from local server's internal API
        const response = await fetch(`${LOCAL_SERVER_URL}/api/internal/public-data/${decryptedId}`);

        if (!response.ok) {
            throw new Error(`Local server returned status ${response.status}`);
        }

        const data = await response.json();
        res.render('public-view', { data });
    } catch (error) {
        console.error('[Global Server] Error fetching public data:', error.message);
        res.status(500).render('public-view', {
            data: { error: 'Error retrieving information. Please try again later.' }
        });
    }
});

// Start server
app.listen(PORT, () => {
    console.log(`\n========================================`);
    console.log(`eckWMS Global Server`);
    console.log(`========================================`);
    console.log(`Running on port: ${PORT}`);
    console.log(`Proxying API requests to: ${LOCAL_SERVER_URL}`);
    console.log(`Environment: ${process.env.NODE_ENV || 'development'}`);
    console.log(`========================================\n`);
});

module.exports = app;
