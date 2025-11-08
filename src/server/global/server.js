require('dotenv').config({ path: require('path').resolve(__dirname, '../../../.env') });
const express = require('express');
const path = require('path');
const { createProxyMiddleware } = require('http-proxy-middleware');
const { betrugerUrlDecrypt } = require('../../shared/utils/encryption');
const db = require('../../shared/models/postgresql');

const app = express();
const PORT = process.env.GLOBAL_SERVER_PORT || 8080;
const LOCAL_SERVER_URL = process.env.LOCAL_SERVER_INTERNAL_URL || 'http://localhost:3000';
const INTERNAL_API_KEY = process.env.GLOBAL_SERVER_API_KEY || 'a_super_secret_key_for_internal_sync';

app.set('view engine', 'ejs');
app.set('views', path.join(__dirname, 'views'));
app.use(express.json());

// --- API Proxy Middleware (for Android client fallback) ---
app.use('/api/proxy', createProxyMiddleware({ target: LOCAL_SERVER_URL, changeOrigin: true, pathRewrite: { '^/api/proxy': '' }, logLevel: 'debug' }));

// --- Internal Sync API (for local server to push data) ---
const internalApiAuth = (req, res, next) => {
  if (req.get('X-Internal-Api-Key') !== INTERNAL_API_KEY) {
    return res.status(403).json({ error: 'Forbidden: Invalid internal API key' });
  }
  next();
};

app.post('/api/internal/sync', internalApiAuth, async (req, res) => {
  const { id, type, data } = req.body;
  if (!id || !type || !data) {
    return res.status(400).json({ error: 'Invalid sync data' });
  }
  try {
    await db.PublicData.upsert({ id, type, data });
    console.log(`[Global Server] Synced data for ${type} ID: ${id}`);
    res.status(200).json({ success: true });
  } catch (error) {
    console.error('[Global Server] Error during data sync:', error.message);
    res.status(500).json({ error: 'Failed to sync data' });
  }
});

// --- Public QR Code Route (reads from own DB) ---
app.get('/:code', async (req, res) => {
  const { code } = req.params;
  if (code === 'favicon.ico') return res.status(204).end();

  const decryptedId = betrugerUrlDecrypt(`ECK1.COM/${code}M3`);
  if (!decryptedId) {
    return res.status(404).render('public-view', { data: { error: 'Code not valid or expired.' } });
  }

  try {
    const publicData = await db.PublicData.findByPk(decryptedId);
    if (!publicData) {
      return res.status(404).render('public-view', { data: { error: 'Information not yet available or code is invalid.' } });
    }
    res.render('public-view', { data: { id: publicData.id, type: publicData.type, ...publicData.data } });
  } catch (error) {
    console.error('[Global Server] Error fetching public data from DB:', error);
    res.status(500).render('public-view', { data: { error: 'Error retrieving information.' } });
  }
});

// --- Server Initialization ---
async function startServer() {
  try {
    await db.sequelize.authenticate();
    console.log('[Global Server] PostgreSQL connection established.');
    await db.sequelize.sync({ alter: process.env.NODE_ENV === 'development' });
    console.log('[Global Server] Models synchronized.');

    app.listen(PORT, () => {
      console.log(`\n========================================`);
      console.log(`eckWMS Global Server`);
      console.log(`========================================`);
      console.log(`Running on port: ${PORT}`);
      console.log(`Proxying API requests to: ${LOCAL_SERVER_URL}`);
      console.log(`========================================\n`);
    });
  } catch (error) {
    console.error('[Global Server] FAILED to start:', error);
    process.exit(1);
  }
}

startServer();
