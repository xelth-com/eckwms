const express = require('express');
const router = express.Router();
const qrcode = require('qrcode');
const nacl = require('tweetnacl');
const { Buffer } = require('node:buffer');
const { requireAdmin } = require('../middleware/auth');
const { RegisteredDevice } = require('../../../shared/models/postgresql');
const { getLocalIpAddresses } = require('../utils/networkUtils');
const { verifyJWT } = require('../../../shared/utils/encryption');

// Endpoint to generate a pairing QR code (requires admin authentication)
// Uses ECK-P1-ALPHA v1.1 protocol: ECK$1$COMPACTUUID$PUBKEY_HEX$URL (Uppercase Alphanumeric Mode)
router.get('/pairing-qr', requireAdmin, async (req, res) => {
  const { type } = req.query; // Check if type=vip
  try {
    const instanceId = process.env.INSTANCE_ID;
    const serverPublicKey = process.env.SERVER_PUBLIC_KEY;
    const globalServerUrl = process.env.GLOBAL_SERVER_URL || 'HTTPS://PDA.REPAIR';

    if (!serverPublicKey) {
      return res.status(500).json({ error: 'Server public key is not configured.' });
    }

    if (!instanceId) {
      return res.status(500).json({ error: 'Instance ID is not configured. Run: npm run generate:id' });
    }

    // Compact UUID: Remove dashes and convert to uppercase for QR density
    const compactUuid = instanceId.replace(/-/g, '').toUpperCase();

    // Convert base64 public key to HEX uppercase for QR Alphanumeric mode
    const publicKeyBuffer = Buffer.from(serverPublicKey, 'base64');
    const publicKeyHex = publicKeyBuffer.toString('hex').toUpperCase();
    const globalUrlUppercase = globalServerUrl.toUpperCase();

    let pairingString = `ECK$1$${compactUuid}$${publicKeyHex}$${globalUrlUppercase}`;

    // Generate VIP Token if requested
    if (type === 'vip') {
        const { generateJWT } = require('../../../shared/utils/encryption');
        const tokenPayload = {
            type: 'invite',
            iat: Math.floor(Date.now() / 1000),
            exp: Math.floor(Date.now() / 1000) + (24 * 60 * 60) // 24 hours valid
        };
        const inviteToken = generateJWT(tokenPayload);
        // Append token as 6th argument
        pairingString += `$${inviteToken}`;
    }

    const qrCodeDataUrl = await qrcode.toDataURL(pairingString);

    res.json({
      success: true,
      qr_code_data_url: qrCodeDataUrl
    });

  } catch (error) {
    console.error('Error generating pairing QR code:', error);
    res.status(500).json({ error: 'Failed to generate QR code.' });
  }
});

// Endpoint for a device to register itself with the server
router.post('/register-device', async (req, res) => {
  const { deviceId, deviceName, devicePublicKey, signature, inviteToken } = req.body;

  if (!deviceId || !devicePublicKey || !signature) {
    return res.status(400).json({ error: 'deviceId, devicePublicKey, and signature are required.' });
  }

  try {
    // Verify the signature
    const message = JSON.stringify({ deviceId, devicePublicKey });
    const signatureBytes = Buffer.from(signature, 'base64');
    const messageBytes = Buffer.from(message, 'utf8');
    const publicKeyBytes = Buffer.from(devicePublicKey, 'base64');

    if (!nacl.sign.detached.verify(messageBytes, signatureBytes, publicKeyBytes)) {
      return res.status(403).json({ error: 'Invalid signature. Device registration failed.' });
    }

    // Determine requested status based on Invite Token
    let requestedStatus = 'pending';
    if (inviteToken) {
      try {
        const tokenPayload = verifyJWT(inviteToken);
        if (tokenPayload && tokenPayload.type === 'invite') {
           requestedStatus = 'active';
           console.log(`[Device Registration] Valid invite token used. Requested status: ACTIVE.`);
        }
      } catch (e) {
        console.error('[Device Registration] Token verification failed:', e);
      }
    }

    // Find existing device first
    const existingDevice = await RegisteredDevice.findOne({ where: { deviceId } });

    if (existingDevice) {
      // SMART LOGIC: Preserve existing status to prevent lockout
      let newStatus = existingDevice.status;

      // Only upgrade to active if it was pending AND we have a valid token
      if (existingDevice.status === 'pending' && requestedStatus === 'active') {
          newStatus = 'active';
      }

      // If device was somehow deleted/unregistered but ID remains (edge case), or we want to force update info
      existingDevice.publicKey = devicePublicKey;
      existingDevice.deviceName = deviceName || existingDevice.deviceName;
      existingDevice.is_active = true;
      existingDevice.status = newStatus;

      await existingDevice.save();

      console.log(`[Device Registration] Existing device updated: ${deviceId}. Status kept as: ${newStatus}`);
      return res.status(200).json({
          success: true,
          message: 'Device registration updated.',
          status: newStatus
      });
    }

    // New Device
    const newDevice = await RegisteredDevice.create({
        deviceId,
        publicKey: devicePublicKey,
        deviceName: deviceName || null,
        is_active: true,
        status: requestedStatus
    });

    console.log(`[Device Registration] NEW device registered: ${deviceId}. Status: ${requestedStatus}`);
    res.status(201).json({
        success: true,
        message: 'Device registered successfully.',
        status: requestedStatus
    });

  } catch (error) {
    console.error('Error registering device:', error);
    res.status(500).json({ error: 'Failed to register device.' });
  }
});

// Endpoint for the admin UI to check the global server's status (public - no auth required)
router.get('/global-server-status', async (req, res) => {
  const globalServerUrl = process.env.GLOBAL_SERVER_URL;
  if (!globalServerUrl) {
    return res.json({ status: 'offline', error: 'GLOBAL_SERVER_URL not configured in .env' });
  }

  try {
    // We add a timeout to prevent long waits
    const controller = new AbortController();
    const timeoutId = setTimeout(() => controller.abort(), 5000); // 5-second timeout

    const response = await fetch(`${globalServerUrl}/health`, { signal: controller.signal });
    clearTimeout(timeoutId);

    if (response.ok) {
      res.json({ status: 'online' });
    } else {
      res.json({ status: 'offline', error: `Responded with status ${response.status}` });
    }
  } catch (error) {
    res.json({ status: 'offline', error: error.message });
  }
});

module.exports = router;
