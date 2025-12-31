// Main Express application for eckwms
require('dotenv').config();

const express = require('express');
const path = require('path');
const fs = require('fs');
const { resolve } = require('path');
const app = express();
const bodyParser = require('body-parser');
const helmet = require('helmet');
const passport = require('passport');
const initI18n = require('./middleware/i18n');
const createLanguageMiddleware = require('./middleware/languageMiddleware');
const { requireAdmin } = require('./middleware/auth');
const i18next = require('i18next');
const createHtmlTranslationInterceptor = require('./middleware/htmlTranslationInterceptor');
const { createProxyMiddleware } = require('http-proxy-middleware');
const { collectAndReportDiagnostics } = require('./utils/startupDiagnostics');
const WebSocket = require('ws');
const { isDuplicate } = require('./utils/messageDeduplicator');

// Services
const inventoryService = require('./services/inventoryService');
const { RmaRequest } = require('../../shared/models/postgresql');

// Import routes
const apiRoutes = require('./routes/api');
const rmaRoutes = require('./routes/rma');
const statusRoutes = require('./routes/status');
const adminRoutes = require('./routes/admin');
const authRoutes = require('./routes/auth');
const translationApiRoutes = require('./routes/translation-api');
const translationAdminRoutes = require('./routes/translation-admin');
const eckwmsRoutes = require('./routes/eckwms');
const uploadRoutes = require('./routes/upload');
const setupRoutes = require('./routes/setup');

const { errorHandler, requestLogger } = require('./middleware');
const db = require('../../shared/models/postgresql');

global.runOnServer = Object.hasOwn(process.env, 'pm_id');
global.baseDirectory = __dirname + '/';

if (!process.env.JWT_SECRET) {
    throw new Error("Critical Error: JWT_SECRET not found in .env!");
}
global.secretJwt = process.env.JWT_SECRET;

// --- Middleware ---
app.use((req, res, next) => {
    console.log('============================= Request Start =========================================');
    next();
});

app.use(helmet({
    contentSecurityPolicy: false,
    crossOriginEmbedderPolicy: false
}));

app.use(express.json());
app.use(express.urlencoded({ extended: true }));
app.use(bodyParser.text({ type: 'text/html' }));
app.use(requestLogger);

const configPassport = require('./config/passport');
configPassport(passport);
app.use(passport.initialize());

app.use(initI18n());
app.use(createHtmlTranslationInterceptor(i18next));
app.use(createLanguageMiddleware());

// --- Static Files ---
app.use(express.static(path.join(__dirname, '../../../public')));

// --- NodeBB Proxy ---
const nodebbTarget = process.env.NODEBB_URL || 'http://localhost:4567';
app.use('/forum', createProxyMiddleware({
  target: nodebbTarget,
  changeOrigin: true,
  ws: true,
  pathRewrite: { '^/forum': '' },
  logLevel: process.env.NODE_ENV === 'development' ? 'debug' : 'info',
  onError: (err, req, res) => {
    console.error('[NodeBB Proxy] Error:', err);
    if (res && !res.headersSent) res.writeHead(502, { 'Content-Type': 'text/plain' });
    if (res && !res.writableEnded) res.end('Proxy Error: Could not connect to NodeBB.');
  }
}));

// --- Routes ---
app.use('/api', apiRoutes);
app.use('/rma', rmaRoutes);
app.use('/status', statusRoutes);
app.use('/admin', adminRoutes);
app.use('/auth', authRoutes);
app.use('/api', translationApiRoutes);
app.use('/translation-admin', requireAdmin, translationAdminRoutes);
app.use('/ECK', eckwmsRoutes);
app.use('/ECK/api/upload', uploadRoutes);
app.use('/api/internal', setupRoutes);
app.use('/api/rbac', require('./routes/rbac'));

app.get('/', (req, res) => res.sendFile(path.join(__dirname, 'html', 'index.html')));
app.get('/health', (req, res) => res.status(200).json({ status: 'ok', server: 'local' }));

// --- Public Data API (Refactored for DB) ---
app.get('/api/internal/public-data/:id', async (req, res) => {
    const { id } = req.params;
    console.log(`[Local Server] Public API request for ID: ${id}`);
    try {
        let data = null;
        let type = null;

        // 1. Check Items
        if (await inventoryService.exists('item', id)) {
            const item = await inventoryService.get('item', id);
            type = 'item';
            data = { id, type, model: item.cl || 'Unknown', status: item.status || 'Unknown', timestamp: item.sn ? new Date(item.sn[1] * 1000).toISOString() : new Date().toISOString() };
        }
        // 2. Check Boxes
        else if (await inventoryService.exists('box', id)) {
            const box = await inventoryService.get('box', id);
            type = 'box';
            data = { id, type, status: box.status || 'Unknown', timestamp: box.sn ? new Date(box.sn[1] * 1000).toISOString() : new Date().toISOString() };
        }
        // 3. Check Orders (RMA)
        else {
            // Try to find RMA by order code pattern
            const rma = await RmaRequest.findOne({ where: { orderCode: id } }) ||
                        await RmaRequest.findOne({ where: { rmaCode: id.replace('o000', '') } });

            if (rma) {
                type = 'order';
                data = { id, type, status: rma.status, timestamp: rma.createdAt };
            }
        }

        if (data) return res.json(data);
        return res.status(404).json({ error: 'Not found', message: 'No information available.' });

    } catch (error) {
        console.error('[Local Server] Error in public API:', error);
        return res.status(500).json({ error: 'Internal server error' });
    }
});

app.use(errorHandler);

// --- Initialization ---
async function initialize() {
    try {
        await db.sequelize.authenticate();
        console.log('PostgreSQL connection established.');

        if (process.env.NODE_ENV === 'development') {
            await db.sequelize.sync({ alter: process.env.DB_ALTER === 'true' });
            console.log('PostgreSQL models synchronized.');
        }

        // Graceful Shutdown
        const shutdown = async () => {
            console.log('\nShutting down...');
            await db.sequelize.close();
            console.log('DB connection closed.');
            process.exit(0);
        };
        process.on('SIGINT', shutdown);
        process.on('SIGTERM', shutdown);

        const PORT = process.env.LOCAL_SERVER_PORT || process.env.PORT || 3100;
        const server = app.listen(PORT, () => {
            console.log(`eckwms server running on port ${PORT}.`);
            if (process.env.NODE_ENV !== 'development-no-sync') collectAndReportDiagnostics();
        });

        // WebSocket Server
        const wss = new WebSocket.Server({ server });
        console.log('[WebSocket] Server initialized');

        global.deviceConnections = new Map();
        global.sendToDevice = (deviceId, type, payload = {}) => {
            const ws = global.deviceConnections.get(deviceId);
            if (ws && ws.readyState === WebSocket.OPEN) {
                ws.send(JSON.stringify({ type, ...payload }));
                return true;
            }
            return false;
        };

        wss.on('connection', (ws, req) => {
            const ip = req.socket.remoteAddress;
            console.log(`[WebSocket] Client connected: ${ip}`);

            ws.on('message', async (message) => {
                try {
                    const data = JSON.parse(message.toString());
                    if (data.type === 'DEVICE_IDENTIFY' && data.deviceId) {
                        global.deviceConnections.set(data.deviceId, ws);
                        ws.deviceId = data.deviceId;
                        ws.send(JSON.stringify({ type: 'ACK', msgId: data.msgId, message: 'Identity confirmed' }));
                        return;
                    }
                    // Standard scan processing
                    const { processScan } = require('./utils/scanHandler');
                    if (data.barcode) {
                        if (isDuplicate(data.msgId)) {
                            ws.send(JSON.stringify({ success: true, duplicate: true, msgId: data.msgId }));
                            return;
                        }
                        const result = await processScan(data.barcode);
                        ws.send(JSON.stringify({ success: true, msgId: data.msgId, ...result }));
                    }
                } catch (e) { console.error('WS Error:', e); }
            });

            ws.on('close', () => {
                if (ws.deviceId) global.deviceConnections.delete(ws.deviceId);
            });
        });

    } catch (err) {
        console.error('FATAL ERROR:', err);
        process.exit(1);
    }
}

initialize();
module.exports = app;
