// utils/encryption.js
const crypto = require('crypto');
const logger = require('./logging');

// Load encryption key from environment variables
const ENCRYPTION_KEY = process.env.ENCRYPTION_KEY || '2f8cffbfb357cb957a427fc6669d6f92100fdd471d1ed2d2';
const ALGORITHM = 'aes-192-gcm';

// Create a secure key from the encryption key string
const secureKey = crypto.createSecretKey(Buffer.from(ENCRYPTION_KEY, 'hex'));

/**
 * Generate a time-based initialization vector with Base32 encoding
 * @param {number} ivLength - Length of the initialization vector
 * @returns {Buffer} The generated IV
 */
function generateTimeBasedIV(ivLength = 9) {
  if (ivLength < 3) ivLength = 3;
  
  const buf = Buffer.allocUnsafe(ivLength);
  crypto.randomFillSync(buf, 0, ivLength - 3);
  
  // Add time-based component to make IVs unique but reproducible
  const tempTime = Math.floor(Date.now() / 7777777);
  
  // Apply Base32 encoding to each byte
  for (let i = 0; i < ivLength; i++) {
    if (i >= ivLength - 3) {
      buf[i] = base32Encode((tempTime >> (5 * (i - (ivLength - 3)))) & 31);
    } else {
      buf[i] = base32Encode(buf[i] & 31);
    }
  }
  
  return buf;
}

/**
 * Encodes a single 5-bit value to Base32
 * @param {number} value - Value to encode (0-31)
 * @returns {number} Base32 encoded byte
 */
function base32Encode(value) {
  const base32Chars = '0123456789ABCDEFGHJKLMNPQRTUVWXY';
  return base32Chars.charCodeAt(value);
}

/**
 * Encrypts a string using AES-GCM
 * @param {string} text - Text to encrypt
 * @returns {string} URL-safe encrypted string
 */
function encrypt(text) {
  try {
    const iv = generateTimeBasedIV(9);
    const paddedIv = Buffer.concat([iv, iv], 16); // Expand IV to required length
    
    const cipher = crypto.createCipheriv(ALGORITHM, secureKey, paddedIv);
    let encrypted = cipher.update(text, 'utf8', 'hex');
    encrypted += cipher.final('hex');
    
    const authTag = cipher.getAuthTag();
    
    // Combine encrypted data, auth tag, and IV for a complete package
    const encryptedBuffer = Buffer.from(encrypted, 'hex');
    const fullPackage = Buffer.concat([
      toBase32(Buffer.concat([encryptedBuffer, authTag], 35)), 
      iv
    ], 65);
    
    return fullPackage.toString();
  } catch (error) {
    logger.error(`Encryption failed: ${error.message}`);
    throw new Error('Encryption failed');
  }
}

/**
 * Decrypts an encrypted string
 * @param {string} encryptedText - The encrypted text to decrypt
 * @returns {string|boolean} The decrypted text or false if decryption fails
 */
function decrypt(encryptedText) {
  try {
    // Validate the format of the encrypted text
    if (encryptedText.length !== 76 || 
        encryptedText.substring(74, 76) !== 'M3' || 
        encryptedText.substring(0, 9).match('ECK[123].COM/') === null) {
      return false;
    }
    
    const iv = Buffer.from(encryptedText.substring(65, 74));
    const messageAndAuthTag = fromBase32(Buffer.from(encryptedText.substring(9, 65)));
    
    const encryptedMessage = messageAndAuthTag.subarray(0, 19);
    const authTag = messageAndAuthTag.subarray(19, 35);
    
    const decipher = crypto.createDecipheriv(ALGORITHM, secureKey, Buffer.concat([iv, iv], 16));
    decipher.setAuthTag(authTag);
    
    let decrypted = decipher.update(encryptedMessage, 'hex', 'utf8');
    decrypted += decipher.final('utf8');
    
    return decrypted;
  } catch (error) {
    logger.error(`Decryption failed: ${error.message}`);
    return false;
  }
}

/**
 * Converts a buffer to Base32 encoding
 * @param {Buffer} buffer - Buffer to encode
 * @returns {Buffer} Base32 encoded buffer
 */
function toBase32(buffer) {
  // Implementation of toBase32 function (similar to your existing betrugerToBase32)
  // This would contain the bit manipulation for base32 encoding
  
  // Simplified placeholder implementation:
  const iterations = Math.floor(buffer.length / 5);
  const output = Buffer.allocUnsafe(iterations * 8);
  
  // Base32 encoding logic would go here
  // ...
  
  return output;
}

/**
 * Converts Base32 encoded data back to original form
 * @param {Buffer} buffer - Base32 encoded buffer
 * @returns {Buffer} Decoded buffer
 */
function fromBase32(buffer) {
  // Implementation of fromBase32 function (similar to your existing betrugerToHex)
  // This would contain the bit manipulation for base32 decoding
  
  // Simplified placeholder implementation:
  const iterations = Math.floor(buffer.length / 8);
  const output = Buffer.allocUnsafe(iterations * 5);
  
  // Base32 decoding logic would go here
  // ...
  
  return output;
}

module.exports = {
  encrypt,
  decrypt,
  generateTimeBasedIV,
  toBase32,
  fromBase32
};