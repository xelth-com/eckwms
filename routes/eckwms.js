// routes/eckwms.js
const express = require('express');
const router = express.Router();
const crypto = require('crypto');
const path = require('path');
const { Scan, EckwmsInstance, RegisteredDevice } = require('../models/postgresql');

const PUBLIC_API_KEY = 'public-demo-key-for-eckwms-app';

/**
 * Middleware: Authenticate API key from X-API-Key header
 */
const authenticateApiKey = async (req, res, next) => {
  const apiKey = req.get('X-API-Key');

  if (!apiKey) {
    return res.status(401).json({
      success: false,
      error: 'Missing X-API-Key header'
    });
  }

  try {
    const instance = await EckwmsInstance.findOne({
      where: { api_key: apiKey }
    });

    if (!instance) {
      return res.status(403).json({
        success: false,
        error: 'Invalid API key'
      });
    }

    req.instance = instance;
    req.isPublicMode = (apiKey === PUBLIC_API_KEY);
    next();
  } catch (error) {
    console.error('[eckWMS] API key authentication error:', error);
    res.status(500).json({
      success: false,
      error: 'Authentication server error'
    });
  }
};

/**
 * Calculate SHA256 checksum for payload
 */
function calculateChecksum(payload) {
  return crypto
    .createHash('sha256')
    .update(JSON.stringify(payload))
    .digest('hex');
}

/**
 * Route to serve the eckWMS live feed page
 */
router.get('/', (req, res) => {
  res.sendFile(path.join(__dirname, '../html/eckwms.html'));
});

/**
 * GET /api/scans
 * Public endpoint: Get recent scans from the public demo account only
 */
router.get('/api/scans', async (req, res) => {
  try {
    const publicInstance = await EckwmsInstance.findOne({
      where: { api_key: PUBLIC_API_KEY }
    });

    if (!publicInstance) {
      return res.status(404).json({
        success: false,
        error: 'Public demo instance not found. Run seed script.'
      });
    }

    const scans = await Scan.findAll({
      where: { instance_id: publicInstance.id },
      order: [['createdAt', 'DESC']],
      limit: 100,
      attributes: ['id', 'payload', 'type', 'priority', 'createdAt']
    });

    res.json(scans);
  } catch (error) {
    console.error('[eckWMS] Error fetching public scans:', error);
    res.status(500).json({
      success: false,
      error: 'Server error while fetching scans'
    });
  }
});

/**
 * POST /api/scan
 * Receive scan data from device and buffer it with a checksum
 * For public API key: deviceId is anonymized for privacy
 */
router.post('/api/scan', authenticateApiKey, async (req, res) => {
  try {
    const { payload, deviceId, priority, type } = req.body;

    console.log(`[eckWMS] Raw request body:`, JSON.stringify(req.body));

    if (!payload) {
      return res.status(400).json({
        success: false,
        error: 'Payload is required'
      });
    }

    const checksum = calculateChecksum(payload);

    // For public mode, do not store deviceId for privacy
    const storedDeviceId = req.isPublicMode ? null : (deviceId || null);

    const newScan = await Scan.create({
      payload: typeof payload === 'string' ? payload : JSON.stringify(payload),
      checksum,
      instance_id: req.instance.id,
      deviceId: storedDeviceId,
      priority: priority || 0,
      type: type || null,
      status: 'buffered'
    });

    console.log(`[eckWMS] Scan buffered for instance ${req.instance.name}: payload="${newScan.payload}", type="${newScan.type}", deviceId="${storedDeviceId}"${req.isPublicMode ? ' [PUBLIC MODE - ANONYMIZED]' : ''}`);

    res.status(201).json({
      success: true,
      scan_id: newScan.id,
      checksum: checksum,
      timestamp: newScan.createdAt
    });

  } catch (error) {
    console.error('[eckWMS] Error buffering scan:', error);
    res.status(500).json({
      success: false,
      error: 'Server error while buffering scan'
    });
  }
});

/**
 * GET /api/pull
 * Pull buffered scan data for the authenticated client instance
 * Optional query params:
 *   - limit: number of scans to pull (default: 100)
 *   - priority_min: minimum priority to pull (default: -infinity)
 */
router.get('/api/pull', authenticateApiKey, async (req, res) => {
  try {
    const limit = Math.min(parseInt(req.query.limit) || 100, 1000);
    const priorityMinParam = req.query.priority_min ? parseInt(req.query.priority_min) : null;

    const whereClause = {
      instance_id: req.instance.id,
      status: 'buffered'
    };

    // Only add priority filter if specified
    if (priorityMinParam !== null) {
      whereClause.priority = {
        [require('sequelize').Op.gte]: priorityMinParam
      };
    }

    const scans = await Scan.findAll({
      where: whereClause,
      attributes: ['id', 'payload', 'checksum', 'priority'],
      order: [['priority', 'DESC'], ['createdAt', 'ASC']],
      limit: limit
    });

    // Update status to 'delivered' for pulled scans
    await Scan.update(
      { status: 'delivered' },
      {
        where: {
          id: {
            [require('sequelize').Op.in]: scans.map(s => s.id)
          }
        }
      }
    );

    console.log(`[eckWMS] Pulled ${scans.length} scans for instance ${req.instance.name}`);

    res.json({
      success: true,
      count: scans.length,
      scans: scans.map(scan => ({
        scan_id: scan.id,
        payload: scan.payload,
        checksum: scan.checksum,
        deviceId: scan.deviceId,
        priority: scan.priority,
        created_at: scan.createdAt
      }))
    });

  } catch (error) {
    console.error('[eckWMS] Error pulling scans:', error);
    res.status(500).json({
      success: false,
      error: 'Server error while pulling scans'
    });
  }
});

/**
 * POST /api/confirm
 * Confirm receipt of pulled scans and trigger cleanup
 * Expects: { scan_ids: [id1, id2, ...] }
 */
router.post('/api/confirm', authenticateApiKey, async (req, res) => {
  try {
    const { scan_ids } = req.body;

    if (!Array.isArray(scan_ids) || scan_ids.length === 0) {
      return res.status(400).json({
        success: false,
        error: 'scan_ids array is required'
      });
    }

    // Update confirmed scans to 'confirmed' status
    const [updatedCount] = await Scan.update(
      { status: 'confirmed' },
      {
        where: {
          id: {
            [require('sequelize').Op.in]: scan_ids
          },
          instance_id: req.instance.id
        }
      }
    );

    console.log(`[eckWMS] Confirmed ${updatedCount} scans for instance ${req.instance.name}`);

    // Schedule cleanup of old confirmed scans (retention logic)
    // Scans confirmed more than 7 days ago can be deleted
    const sevenDaysAgo = new Date(Date.now() - 7 * 24 * 60 * 60 * 1000);
    await Scan.destroy({
      where: {
        instance_id: req.instance.id,
        status: 'confirmed',
        updatedAt: {
          [require('sequelize').Op.lt]: sevenDaysAgo
        }
      }
    });

    res.json({
      success: true,
      confirmed_count: updatedCount
    });

  } catch (error) {
    console.error('[eckWMS] Error confirming scans:', error);
    res.status(500).json({
      success: false,
      error: 'Server error while confirming scans'
    });
  }
});

module.exports = router;
