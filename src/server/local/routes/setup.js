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

    // Force URL to uppercase for QR Alphanumeric mode
    const globalUrlUppercase = globalServerUrl.toUpperCase();

    // ECK-P1-ALPHA v1.1 format: ECK$1$COMPACTUUID$PUBKEY_HEX$URL (fully uppercase)
    const pairingString = `ECK$1$${compactUuid}$${publicKeyHex}$${globalUrlUppercase}`;

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
    // Verify the signature to ensure the request is from the device that owns the public key
    const message = JSON.stringify({ deviceId, devicePublicKey });
    const signatureBytes = Buffer.from(signature, 'base64');
    const messageBytes = Buffer.from(message, 'utf8');
    const publicKeyBytes = Buffer.from(devicePublicKey, 'base64');

    if (!nacl.sign.detached.verify(messageBytes, signatureBytes, publicKeyBytes)) {
      return res.status(403).json({ error: 'Invalid signature. Device registration failed.' });
    }

    // Determine initial status based on Invite Token
    let initialStatus = 'pending';
    if (inviteToken) {
      try {
        const tokenPayload = verifyJWT(inviteToken);
        // Check if it's a valid invite token (you can add more logic here later like expiration or limits)
        if (tokenPayload && tokenPayload.type === 'invite') {
           initialStatus = 'active';
           console.log(`[Device Registration] Valid invite token used. Status set to ACTIVE.`);
        } else {
           console.warn(`[Device Registration] Invalid or expired invite token. Status set to PENDING.`);
        }
      } catch (e) {
        console.error('[Device Registration] Token verification failed:', e);
      }
    }

    // Signature is valid, store the device
    const [device, created] = await RegisteredDevice.findOrCreate({
      where: { deviceId: deviceId },
      defaults: {
        publicKey: devicePublicKey,
        deviceName: deviceName || null,
        is_active: true,
        status: initialStatus
      }
    });

    if (!created) {
      // If device already exists, update its public key and name
      device.publicKey = devicePublicKey;
      device.deviceName = deviceName || device.deviceName;
      device.is_active = true;
      // Only upgrade status if a valid token is provided, never downgrade existing active status
      if (initialStatus === 'active') {
          device.status = 'active';
      }
      await device.save();
      console.log(`[Device Registration] Re-registered device: ${deviceId} (Status: ${device.status})`);
      return res.status(200).json({ success: true, message: 'Device re-registered successfully.', status: device.status });
    }

    console.log(`[Device Registration] New device registered: ${deviceId} (Status: ${initialStatus})`);
    res.status(201).json({ success: true, message: 'Device registered successfully.', status: initialStatus });

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
