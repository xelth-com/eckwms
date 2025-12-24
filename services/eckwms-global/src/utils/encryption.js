/**
 * Localized Encryption Utilities for eckWMS Global Server
 *
 * Copied from: pda.repair/utils/encryption.js
 * Purpose: Decrypt QR codes and handle authentication
 *
 * NOTE: This is a localized copy to ensure the microservice is truly independent
 * and doesn't depend on parent directory imports (../../utils/encryption)
 */

const crypto = require('crypto');

// Base32 Table for decoding QR codes
const base32table = '0123456789ABCDEFGHJKLMNPQRTUVWXY';
const base32backHash = {};

for (let i = 0; i < base32table.length; i++) {
  base32backHash[base32table[i]] = i;
}

/**
 * Convert Base32 encoded buffer to hex
 * @param {Buffer} bufIn - Input buffer
 * @returns {Buffer} Decoded hex buffer
 */
function betrugerToHex(bufIn) {
  const iterat = Math.floor(bufIn.length / 8);
  const bufOut = Buffer.allocUnsafe(iterat * 5);

  for (let i = 0; i < iterat; i++) {
    bufOut[i * 5 + 0] =
      (base32backHash[String.fromCharCode(bufIn[i * 8 + 0])] << 3) +
      (base32backHash[String.fromCharCode(bufIn[i * 8 + 1])] >> 2);
    bufOut[i * 5 + 1] =
      (base32backHash[String.fromCharCode(bufIn[i * 8 + 1])] << 6) +
      (base32backHash[String.fromCharCode(bufIn[i * 8 + 2])] << 1) +
      (base32backHash[String.fromCharCode(bufIn[i * 8 + 3])] >> 4);
    bufOut[i * 5 + 2] =
      (base32backHash[String.fromCharCode(bufIn[i * 8 + 3])] << 4) +
      (base32backHash[String.fromCharCode(bufIn[i * 8 + 4])] >> 1);
    bufOut[i * 5 + 3] =
      (base32backHash[String.fromCharCode(bufIn[i * 8 + 4])] << 7) +
      (base32backHash[String.fromCharCode(bufIn[i * 8 + 5])] << 2) +
      (base32backHash[String.fromCharCode(bufIn[i * 8 + 6])] >> 3);
    bufOut[i * 5 + 4] =
      (base32backHash[String.fromCharCode(bufIn[i * 8 + 6])] << 5) +
      base32backHash[String.fromCharCode(bufIn[i * 8 + 7])];
  }

  return bufOut;
}

/**
 * Decrypt QR code using AES-192-GCM
 *
 * @param {string} betrugerUrl - Encrypted QR code string
 * @returns {string|boolean} Decrypted ID or false if decryption fails
 */
exports.betrugerUrlDecrypt = (betrugerUrl) => {
  try {
    const encKey = process.env.ENC_KEY;
    if (!encKey) {
      console.warn('[eckWMS Encryption] ENC_KEY not configured in environment');
      return false;
    }

    const decryptionKey = crypto.createSecretKey(encKey, 'hex');
    const algorithm = 'aes-192-gcm';

    // Validate input format
    if (typeof betrugerUrl !== 'string' || betrugerUrl.length !== 76) {
      return false;
    }

    // Extract components
    const betIv = Buffer.from(betrugerUrl.substring(65, 74));
    const base32Data = Buffer.from(betrugerUrl.substring(9, 65));
    const decodedData = betrugerToHex(base32Data);

    if (decodedData.length <= 16) return false;

    const authTagLength = 16;
    const recMes = decodedData.subarray(0, decodedData.length - authTagLength);
    const recATag = decodedData.subarray(decodedData.length - authTagLength);

    // Create IV (concatenate betIv with itself to reach 16 bytes)
    const iv = Buffer.concat([betIv, betIv], 16);

    // Decrypt
    const decipher = crypto.createDecipheriv(algorithm, decryptionKey, iv);
    decipher.setAuthTag(recATag);
    let decrypted = decipher.update(recMes);
    decrypted = Buffer.concat([decrypted, decipher.final()]);

    return decrypted.toString('utf8');
  } catch (error) {
    console.error('[eckWMS Encryption] Decryption error:', error.message);
    return false;
  }
};

// Eck branding aliases (same functionality, new naming)
exports.eckUrlDecrypt = exports.betrugerUrlDecrypt;
exports.eckToHex = betrugerToHex;

/**
 * Simple API key validation (can be enhanced with JWT later)
 * @param {string} apiKey - API key to validate
 * @returns {boolean} True if valid
 */
exports.validateApiKey = (apiKey) => {
  const expectedKey = process.env.GLOBAL_SERVER_API_KEY;
  return expectedKey && apiKey === expectedKey;
};
