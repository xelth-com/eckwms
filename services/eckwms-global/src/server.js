/**
 * eckWMS Global Server
 *
 * Standalone Express application serving global-facing endpoints:
 * - Public QR codes
 * - Instance management & discovery
 * - API proxy for local instances
 * - Health checks
 *
 * This is a truly independent microservice with:
 * ✓ Local models (no imports from parent)
 * ✓ Local utilities (no imports from parent)
 * ✓ Independent package.json
 * ✓ Separate environment configuration
 * ✓ Container-ready
 */

require('dotenv').config({ path: require('path').resolve(__dirname, '../.env') });

const express = require('express');
const path = require('path');
const { createProxyMiddleware } = require('http-proxy-middleware');

// LOCAL IMPORTS (no relative paths to parent directories)
const db = require('./models');
const { betrugerUrlDecrypt, validateApiKey } = require('./utils/encryption');

const app = express();
const PORT = process.env.PORT || process.env.GLOBAL_SERVER_PORT || 8080;
const INTERNAL_API_KEY = process.env.GLOBAL_SERVER_API_KEY;

// --- View Engine Configuration ---
app.set('view engine', 'ejs');
app.set('views', path.join(__dirname, '../views'));

// --- Middleware ---
app.use(express.json());
app.use(express.urlencoded({ extended: true }));

// Request logging
app.use((req, res, next) => {
  console.log(`[eckWMS] ${req.method} ${req.path}`);
  next();
});

console.log('\n╔════════════════════════════════════════════════════════╗');
console.log('║                                                        ║');
console.log('║     eckWMS Global Server - Starting                   ║');
console.log('║                                                        ║');
console.log('╚════════════════════════════════════════════════════════╝');
console.log(`[eckWMS] Port: ${PORT}`);
console.log(`[eckWMS] Environment: ${process.env.NODE_ENV || 'development'}`);
console.log(`[eckWMS] Database: ${process.env.PG_HOST}:${process.env.PG_PORT || 5432}/${process.env.PG_DATABASE || 'eckwms_global'}\n`);

// --- Routes ---
const eckRouter = express.Router();

/**
 * 1. Health Check Endpoint
 * Provides service health status for monitoring
 */
eckRouter.get('/health', async (req, res) => {
  try {
    const dbHealth = db && db.sequelize ? await db.sequelize.authenticate().then(() => 'connected').catch(() => 'disconnected') : 'not-configured';

    res.status(200).json({
      status: dbHealth === 'connected' ? 'healthy' : 'degraded',
      service: 'eckWMS Global Server',
      timestamp: new Date().toISOString(),
      uptime: process.uptime(),
      database: dbHealth,
      version: '1.0.0'
    });
  } catch (error) {
    console.error('[eckWMS] Health check error:', error.message);
    res.status(503).json({
      status: 'unhealthy',
      error: error.message
    });
  }
});

/**
 * 2. API Proxy for Local Instances
 * Forwards requests to local eckWMS instances
 * Required header: X-eckWMS-Target-Url
 */
eckRouter.use('/proxy', (req, res, next) => {
  const target = req.headers['x-eckwms-target-url'];

  if (!target) {
    console.warn('[eckWMS] Proxy request missing X-eckWMS-Target-Url header');
    return res.status(400).json({
      error: 'Missing X-eckWMS-Target-Url header',
      message: 'Please provide the target URL in the X-eckWMS-Target-Url header',
      example: 'X-eckWMS-Target-Url: http://192.168.1.100:3000'
    });
  }

  console.log(`[eckWMS] Proxying to: ${target}`);

  const proxyMiddleware = createProxyMiddleware({
    target: target,
    changeOrigin: true,
    pathRewrite: { '^/ECK/proxy': '' },
    logLevel: process.env.NODE_ENV === 'development' ? 'debug' : 'warn',
    onError: (err, req, res) => {
      console.error('[eckWMS] Proxy error:', err.message);
      res.status(502).json({
        error: 'Proxy failed',
        message: err.message,
        targetUrl: target
      });
    }
  });

  proxyMiddleware(req, res, next);
});

/**
 * 3. Internal API Authentication Middleware
 * Validates X-Internal-Api-Key header for protected endpoints
 */
const internalApiAuth = (req, res, next) => {
  if (!INTERNAL_API_KEY) {
    console.warn('[eckWMS] GLOBAL_SERVER_API_KEY not configured');
    return res.status(500).json({
      error: 'Server misconfiguration',
      message: 'GLOBAL_SERVER_API_KEY environment variable not set'
    });
  }

  const providedKey = req.get('X-Internal-Api-Key');
  if (providedKey !== INTERNAL_API_KEY) {
    console.warn(`[eckWMS] Unauthorized API access attempt from ${req.ip}`);
    return res.status(403).json({
      error: 'Forbidden',
      message: 'Invalid or missing internal API key'
    });
  }

  next();
};

/**
 * 4. Instance Registration Endpoint
 * Registers a new eckWMS instance
 *
 * POST /ECK/api/internal/register-instance
 * Headers: X-Internal-Api-Key
 * Body: { instanceId, serverPublicKey, localIps, tracerouteToGlobal }
 */
eckRouter.post('/api/internal/register-instance', internalApiAuth, async (req, res) => {
  const { instanceId, serverPublicKey, localIps, tracerouteToGlobal } = req.body;

  if (!instanceId) {
    return res.status(400).json({
      error: 'Missing required field',
      message: 'instanceId is required'
    });
  }

  const publicIp = req.ip || req.headers['x-forwarded-for'] || 'unknown';

  try {
    console.log(`[eckWMS] Instance registration - ID: ${instanceId}, IP: ${publicIp}`);

    // Find or create instance
    const [instance, created] = await db.EckwmsInstance.findOrCreate({
      where: { id: instanceId },
      defaults: {
        id: instanceId,
        name: `Instance ${instanceId.substring(0, 8)}`,
        server_url: `http://${publicIp}:${process.env.LOCAL_SERVER_PORT || 3000}`,
        api_key: `key_${instanceId.substring(0, 16)}`,
        tier: 'free'
      }
    });

    // Update instance with latest info
    instance.publicIp = publicIp;
    instance.localIps = localIps || [];
    instance.tracerouteToGlobal = tracerouteToGlobal || null;
    instance.serverPublicKey = serverPublicKey || null;
    instance.lastSeen = new Date();
    await instance.save();

    res.status(200).json({
      success: true,
      message: `Instance ${created ? 'registered' : 'updated'} successfully`,
      instanceId: instance.id,
      detectedIp: publicIp
    });
  } catch (error) {
    console.error('[eckWMS] Registration error:', error.message);
    res.status(500).json({
      error: 'Registration failed',
      message: error.message
    });
  }
});

/**
 * 5. Get Instance Info Endpoint
 * Retrieves information about a registered instance
 *
 * GET /ECK/api/internal/get-instance-info/:id
 * Headers: X-Internal-Api-Key
 */
eckRouter.get('/api/internal/get-instance-info/:id', internalApiAuth, async (req, res) => {
  const { id } = req.params;

  try {
    console.log(`[eckWMS] Fetching instance info for: ${id}`);

    const instance = await db.EckwmsInstance.findByPk(id);

    if (!instance) {
      return res.status(404).json({
        error: 'Instance not found',
        message: `No instance registered with ID: ${id}`
      });
    }

    // Build connection candidates
    const candidates = [];

    // Priority 1: Local IPs
    if (instance.localIps && instance.localIps.length > 0) {
      instance.localIps.forEach(ip => {
        candidates.push({
          url: `http://${ip}:${process.env.LOCAL_SERVER_PORT || 3000}`,
          type: 'LOCAL_LAN',
          priority: 1,
          reason: 'Reported by server as local IP'
        });
      });
    }

    // Priority 2: Public IP
    if (instance.publicIp) {
      candidates.push({
        url: `http://${instance.publicIp}:${process.env.LOCAL_SERVER_PORT || 3000}`,
        type: 'PUBLIC_IP',
        priority: 2,
        reason: 'Public IP of the instance'
      });
    }

    // Priority 3: Global proxy fallback
    candidates.push({
      url: `${process.env.GLOBAL_SERVER_URL || 'https://pda.repair'}/ECK/proxy`,
      type: 'GLOBAL_PROXY',
      priority: 3,
      reason: 'Global proxy - guaranteed fallback'
    });

    res.status(200).json({
      instanceId: instance.id,
      name: instance.name,
      tier: instance.tier,
      serverPublicKey: instance.serverPublicKey,
      connectionCandidates: candidates,
      lastSeen: instance.lastSeen
    });
  } catch (error) {
    console.error('[eckWMS] Instance info error:', error.message);
    res.status(500).json({
      error: 'Failed to retrieve instance info',
      message: error.message
    });
  }
});

/**
 * 6. Internal Data Sync Endpoint
 * Syncs data between instances and global server
 *
 * POST /ECK/api/internal/sync
 * Headers: X-Internal-Api-Key
 * Body: { id, type, data }
 */
eckRouter.post('/api/internal/sync', internalApiAuth, async (req, res) => {
  const { id, type, data } = req.body;

  if (!id || !type || !data) {
    return res.status(400).json({
      error: 'Invalid sync data',
      message: 'id, type, and data fields are required'
    });
  }

  try {
    console.log(`[eckWMS] Syncing data - id: ${id}, type: ${type}`);

    // TODO: Implement data sync logic
    // For now, just acknowledge the sync
    res.status(200).json({
      success: true,
      message: 'Data synced successfully',
      syncedItem: { id, type }
    });
  } catch (error) {
    console.error('[eckWMS] Sync error:', error.message);
    res.status(500).json({
      error: 'Sync failed',
      message: error.message
    });
  }
});

/**
 * 7. Public QR Code Page
 * Serves public-facing QR code information pages
 *
 * GET /ECK/:code
 */
eckRouter.get('/:code', async (req, res) => {
  const { code } = req.params;

  // Ignore favicon and reserved paths
  if (['favicon.ico', 'health', 'proxy', 'api'].includes(code.toLowerCase()) || code.startsWith('api/')) {
    return res.status(404).send();
  }

  console.log(`[eckWMS] QR code request: ${code}`);

  try {
    // Decrypt QR code
    const decryptedId = betrugerUrlDecrypt(`ECK/${code}M3`);

    if (!decryptedId) {
      console.warn(`[eckWMS] Invalid QR code: ${code}`);
      return res.status(404).render('eck-public-view', {
        data: { error: 'Code not valid or expired.' }
      });
    }

    console.log(`[eckWMS] QR code decrypted: ${decryptedId}`);

    // TODO: Fetch actual data from database when PublicData model is created
    // For now, return stub response
    res.render('eck-public-view', {
      data: {
        id: decryptedId,
        type: 'qrcode',
        message: 'QR code information page'
      }
    });
  } catch (error) {
    console.error('[eckWMS] QR code error:', error.message);
    res.status(500).render('eck-public-view', {
      data: { error: 'Error retrieving information' }
    });
  }
});

// Mount all ECK routes
app.use('/ECK', eckRouter);

/**
 * Root endpoint - API documentation
 */
app.get('/', (req, res) => {
  res.status(200).json({
    server: 'eckWMS Global Server',
    version: '1.0.0',
    status: 'running',
    documentation: 'https://docs.pda.repair/eckwms',
    endpoints: {
      health: 'GET /ECK/health',
      publicQr: 'GET /ECK/:code',
      registerInstance: 'POST /ECK/api/internal/register-instance (requires X-Internal-Api-Key)',
      getInstanceInfo: 'GET /ECK/api/internal/get-instance-info/:id (requires X-Internal-Api-Key)',
      proxy: 'POST /ECK/proxy (requires X-eckWMS-Target-Url header)',
      sync: 'POST /ECK/api/internal/sync (requires X-Internal-Api-Key)'
    }
  });
});

/**
 * 404 Handler
 */
app.use((req, res) => {
  res.status(404).json({
    error: 'Not found',
    path: req.path,
    method: req.method
  });
});

/**
 * Global Error Handler
 */
app.use((err, req, res, next) => {
  console.error('[eckWMS] Unhandled error:', err);
  res.status(500).json({
    error: 'Internal server error',
    message: process.env.NODE_ENV === 'development' ? err.message : 'An error occurred',
    ...(process.env.NODE_ENV === 'development' && { stack: err.stack })
  });
});

/**
 * Server Initialization
 */
async function startServer() {
  try {
    // Database setup (optional - can run without DB in stub mode)
    if (process.env.PG_HOST && process.env.PG_DATABASE) {
      console.log('[eckWMS] Initializing database connection...');
      await db.sequelize.authenticate();
      console.log('[eckWMS] ✓ PostgreSQL connection established');

      // Sync models
      const syncOptions = process.env.NODE_ENV === 'development' ? { alter: false } : {};
      await db.sequelize.sync(syncOptions);
      console.log('[eckWMS] ✓ Database models synchronized');
    } else {
      console.warn('[eckWMS] ⚠ Database not configured - running in stub mode');
    }

    // Start Express server
    app.listen(PORT, '0.0.0.0', () => {
      console.log(`\n[eckWMS] ✓ Server running on http://0.0.0.0:${PORT}`);
      console.log(`[eckWMS] ✓ Health check: http://localhost:${PORT}/ECK/health`);
      console.log(`[eckWMS] ✓ API endpoints: http://localhost:${PORT}/ECK/*`);
      console.log('\n');
    });
  } catch (error) {
    console.error('[eckWMS] FATAL - Failed to start server:', error);
    process.exit(1);
  }
}

/**
 * Graceful Shutdown
 */
const gracefulShutdown = async (signal) => {
  console.log(`\n[eckWMS] Received ${signal}. Shutting down gracefully...`);
  try {
    if (db && db.sequelize) {
      await db.sequelize.close();
      console.log('[eckWMS] ✓ Database connection closed');
    }
    process.exit(0);
  } catch (error) {
    console.error('[eckWMS] Error during shutdown:', error);
    process.exit(1);
  }
};

process.on('SIGINT', () => gracefulShutdown('SIGINT'));
process.on('SIGTERM', () => gracefulShutdown('SIGTERM'));

// Start the server
startServer();

module.exports = app;
