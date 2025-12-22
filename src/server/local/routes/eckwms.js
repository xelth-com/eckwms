// routes/eckwms.js
const express = require('express');
const router = express.Router();
const crc32 = require('buffer-crc32');
const path = require('path');
const { Scan, EckwmsInstance, RegisteredDevice } = require('../../../shared/models/postgresql');

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
 * Calculate CRC32 checksum for payload
 */
function calculateChecksum(payload) {
  const buffer = Buffer.from(JSON.stringify(payload));
  return crc32.unsigned(buffer).toString(16).padStart(8, '0');
}

/**
 * Route to serve the eckWMS live feed page
 */
router.get('/', (req, res) => {
  res.sendFile(path.join(__dirname, '../html/eckwms.html'));
});

/**
 * GET /API/SCANS
 * Public endpoint: Get recent scans from the public demo account only
 */
router.get('/API/SCANS', async (req, res) => {
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
 * POST /API/SCAN
 * Receive scan data from device and buffer it with a checksum
 * For public API key: deviceId is anonymized for privacy
 */
router.post('/API/SCAN', authenticateApiKey, async (req, res) => {
  try {
    const { payload, deviceId, priority, type } = req.body;

    // STRICT SECURITY CHECK: Validate Device Status
    if (deviceId && !req.isPublicMode) {
      const device = await RegisteredDevice.findOne({ where: { deviceId } });
      if (!device) {
        console.warn(`[Security] Blocked scan from unknown device: ${deviceId}`);
        return res.status(403).json({ success: false, error: 'Device not registered', code: 'DEVICE_NOT_FOUND' });
      }
      if (device.status !== 'active') {
        console.warn(`[Security] Blocked scan from ${device.status} device: ${deviceId}`);
        return res.status(403).json({ success: false, error: `Device is ${device.status}`, code: 'DEVICE_BLOCKED' });
      }
    }

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
 * GET /API/PULL
 * Pull buffered scan data for the authenticated client instance
 * Optional query params:
 *   - limit: number of scans to pull (default: 100)
 *   - priority_min: minimum priority to pull (default: -infinity)
 */
router.get('/API/PULL', authenticateApiKey, async (req, res) => {
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
 * POST /API/CONFIRM
 * Confirm receipt of pulled scans and trigger cleanup
 * Expects: { scan_ids: [id1, id2, ...] }
 */
router.post('/API/CONFIRM', authenticateApiKey, async (req, res) => {
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

/**
 * POST /API/DEVICE/REGISTER
 * Device Registration Endpoint
 * Registers a device with Ed25519 signature verification
 */
router.post('/API/DEVICE/REGISTER', async (req, res) => {
  const { deviceId, deviceName, devicePublicKey, signature } = req.body;
  const nacl = require('tweetnacl');
  const { Buffer } = require('node:buffer');

  if (!deviceId || !devicePublicKey || !signature) {
    return res.status(400).json({ error: 'Missing required fields' });
  }

  try {
    const message = JSON.stringify({ deviceId, devicePublicKey });
    const signatureBytes = Buffer.from(signature, 'base64');
    const messageBytes = Buffer.from(message, 'utf8');
    const publicKeyBytes = Buffer.from(devicePublicKey, 'base64');

    if (!nacl.sign.detached.verify(messageBytes, signatureBytes, publicKeyBytes)) {
      return res.status(403).json({ error: 'Invalid signature' });
    }

    const [device, created] = await RegisteredDevice.findOrCreate({
      where: { deviceId },
      defaults: { publicKey: devicePublicKey, deviceName, is_active: true }
    });

    if (!created) {
      device.publicKey = devicePublicKey;
      device.deviceName = deviceName || device.deviceName;
      device.is_active = true;
      await device.save();
    }

    console.log(`[eckWMS] Device registered: ${deviceId} (${deviceName || 'unnamed'})`);

    res.status(201).json({ success: true, message: 'Device registered' });
  } catch (error) {
    console.error('[eckWMS] Registration error:', error);
    res.status(500).json({ error: 'Internal server error' });
  }
});

/**
 * GET /API/DEVICE/:deviceId/STATUS
 * Lightweight endpoint for devices to poll their authorization status
 * No authentication required - devices need to check their status even when blocked
 */
router.get('/API/DEVICE/:deviceId/STATUS', async (req, res) => {
  try {
    const { deviceId } = req.params;
    const device = await RegisteredDevice.findOne({
      where: { deviceId },
      attributes: ['status', 'is_active', 'deviceName']
    });

    if (!device) {
      // Device was deleted from admin panel
      return res.json({
        status: 'unregistered',
        active: false,
        message: 'Device not found in system'
      });
    }

    res.json({
      status: device.status, // active, pending, blocked
      active: device.is_active,
      name: device.deviceName || 'Unnamed Device'
    });
  } catch (error) {
    console.error('[eckWMS] Status check error:', error);
    res.status(500).json({
      error: 'Internal error',
      status: 'error'
    });
  }
});

/**
 * POST /API/AI/RESPOND
 * Handle user responses to AI questions (feedback loop)
 * Authenticated endpoint for processing user confirmations
 */
router.post('/API/AI/RESPOND', authenticateApiKey, async (req, res) => {
  try {
    const { interactionId, response, barcode, deviceId } = req.body;

    console.log(`[eckWMS] AI Response received: interactionId=${interactionId}, response="${response}", barcode=${barcode}, deviceId=${deviceId}`);

    // Validate required fields
    if (!barcode || !response) {
      return res.status(400).json({
        success: false,
        error: 'Missing required fields: barcode and response are required'
      });
    }

    // STRICT SECURITY CHECK: Validate Device Status (if deviceId provided)
    if (deviceId && !req.isPublicMode) {
      const device = await RegisteredDevice.findOne({ where: { deviceId } });
      if (!device) {
        console.warn(`[Security] Blocked AI response from unknown device: ${deviceId}`);
        return res.status(403).json({ success: false, error: 'Device not registered', code: 'DEVICE_NOT_FOUND' });
      }
      if (device.status !== 'active') {
        console.warn(`[Security] Blocked AI response from ${device.status} device: ${deviceId}`);
        return res.status(403).json({ success: false, error: `Device is ${device.status}`, code: 'DEVICE_BLOCKED' });
      }
    }

    // Process the AI response using scanHandler
    const scanHandler = require('../utils/scanHandler');
    const result = await scanHandler.processAiResponse(barcode, response, deviceId);

    console.log(`[eckWMS] AI Response processed for barcode ${barcode}: ${result.message}`);

    res.status(200).json({
      success: true,
      interactionId: interactionId,
      result: result,
      timestamp: new Date().toISOString()
    });

  } catch (error) {
    console.error('[eckWMS] Error processing AI response:', error);
    res.status(500).json({
      success: false,
      error: 'Server error while processing AI response',
      details: error.message
    });
  }
});

module.exports = router;
