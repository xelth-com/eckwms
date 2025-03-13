// utils/encryption.js
const {
    randomFillSync,
    createCipheriv,
    createDecipheriv,
    createSecretKey,
    createHmac
} = require('node:crypto');
const { Buffer } = require('node:buffer');
const crc32 = require('buffer-crc32');

// Base32 encoding/decoding setup
const base32table = Buffer.from('0123456789ABCDEFGHJKLMNPQRTUVWXY');
const base32backHash = [];
base32table.forEach((element, index) => {
    base32backHash[element] = index;
});

const algorithm = 'aes-192-gcm';

/**
 * Generates a time-based IV using Base32 encoding
 * @param {number} ivLength - Length of the IV to generate
 * @returns {Buffer} - Generated IV
 */
const betrugerTimeIvBase32 = (ivLength) => {
    if (ivLength < 3) ivLength = 3;
    const tempTime = Math.floor(Date.now() / 7777777);
    const buf = Buffer.allocUnsafe(ivLength);
    randomFillSync(buf, 0, ivLength - 3);
    
    let shiftedTime = tempTime;
    buf.forEach((element, index) => {
        if (index >= ivLength - 3) {
            buf[index] = base32table[(shiftedTime >>= 5) & 31];
        } else {
            buf[index] = base32table[element & 31];
        }
    });
    return buf;
};

/**
 * Converts buffer to Base32 encoded buffer
 * @param {Buffer} bufIn - Input buffer
 * @returns {Buffer} - Base32 encoded buffer
 */
const betrugerToBase32 = (bufIn) => {
    const iterat = Math.floor(bufIn.length / 5);
    const bufOut = Buffer.allocUnsafe(iterat * 8);
    
    for (let i = 0; i < iterat; i++) {
        bufOut[i * 8 + 0] = base32table[bufIn[i * 5 + 0] >> 3];
        bufOut[i * 8 + 1] = base32table[((bufIn[i * 5 + 0] << 2) + (bufIn[i * 5 + 1] >> 6)) & 31];
        bufOut[i * 8 + 2] = base32table[(bufIn[i * 5 + 1] >> 1) & 31];
        bufOut[i * 8 + 3] = base32table[((bufIn[i * 5 + 1] << 4) + (bufIn[i * 5 + 2] >> 4)) & 31];
        bufOut[i * 8 + 4] = base32table[((bufIn[i * 5 + 2] << 1) + (bufIn[i * 5 + 3] >> 7)) & 31];
        bufOut[i * 8 + 5] = base32table[(bufIn[i * 5 + 3] >> 2) & 31];
        bufOut[i * 8 + 6] = base32table[((bufIn[i * 5 + 3] << 3) + (bufIn[i * 5 + 4] >> 5)) & 31];
        bufOut[i * 8 + 7] = base32table[(bufIn[i * 5 + 4]) & 31];
    }
    
    return bufOut;
};

/**
 * Converts Base32 encoded buffer back to original format
 * @param {Buffer} bufIn - Base32 encoded buffer
 * @returns {Buffer} - Decoded buffer
 */
const betrugerToHex = (bufIn) => {
    const iterat = Math.floor(bufIn.length / 8);
    const bufOut = Buffer.allocUnsafe(iterat * 5);
    
    for (let i = 0; i < iterat; i++) {
        bufOut[i * 5 + 0] = (base32backHash[bufIn[i * 8 + 0]] << 3) + (base32backHash[bufIn[i * 8 + 1]] >> 2);
        bufOut[i * 5 + 1] = (base32backHash[bufIn[i * 8 + 1]] << 6) + (base32backHash[bufIn[i * 8 + 2]] << 1) + (base32backHash[bufIn[i * 8 + 3]] >> 4);
        bufOut[i * 5 + 2] = (base32backHash[bufIn[i * 8 + 3]] << 4) + (base32backHash[bufIn[i * 8 + 4]] >> 1);
        bufOut[i * 5 + 3] = (base32backHash[bufIn[i * 8 + 4]] << 7) + (base32backHash[bufIn[i * 8 + 5]] << 2) + (base32backHash[bufIn[i * 8 + 6]] >> 3);
        bufOut[i * 5 + 4] = (base32backHash[bufIn[i * 8 + 6]] << 5) + (base32backHash[bufIn[i * 8 + 7]]);
    }
    
    return bufOut;
};

/**
 * Encrypts a string and formats it for use in a URL
 * @param {string} string19 - String to encrypt
 * @param {Buffer} encryptionKey - Encryption key
 * @returns {string} - Encrypted URL-safe string
 */
const betrugerUrlEncrypt = (string19, encryptionKey) => {
    const betIv = betrugerTimeIvBase32(9);
    const iv = Buffer.concat([betIv, betIv], 16);
    const cipher = createCipheriv(algorithm, encryptionKey, iv);
    
    let encrypted = cipher.update(string19, 'utf8', 'hex');
    encrypted += cipher.final('hex');
    const authTag = cipher.getAuthTag();
    
    return Buffer.concat([
        betrugerToBase32(Buffer.concat([Buffer.from(encrypted, 'hex'), authTag], 35)), 
        betIv
    ], 65).toString();
};

/**
 * Decrypts a URL-formatted encrypted string
 * @param {string} betrugerUrl - Encrypted URL to decrypt
 * @param {Buffer} decryptionKey - Decryption key
 * @returns {string|boolean} - Decrypted string or false if decryption fails
 */
const betrugerUrlDecrypt = (betrugerUrl, decryptionKey) => {
    if (betrugerUrl.length === 76 && 
        betrugerUrl.substring(74, 76) === 'M3' && 
        betrugerUrl.substring(0, 9).match('ECK[123].COM/') != null) {
        
        const receiveIv = Buffer.from(betrugerUrl.substring(65, 74));
        const receiveMessageAndAuthTag = betrugerToHex(Buffer.from(betrugerUrl.substring(9, 65)));
        const recMes = receiveMessageAndAuthTag.subarray(0, 19);
        const recATag = receiveMessageAndAuthTag.subarray(19, 35);
        
        const decipher = createDecipheriv(algorithm, decryptionKey, Buffer.concat([receiveIv, receiveIv], 16));
        decipher.setAuthTag(recATag);
        
        let decrypted = decipher.update(recMes, 'hex', 'utf8');
        try {
            decrypted += decipher.final('utf8');
            return decrypted;
        } catch (err) {
            console.log(err.message.replace('Unsupported state or ', ''));
            return false;
        }
    }
    return false;
};

/**
 * Generates a JWT (JSON Web Token)
 * @param {Object} payload - JWT payload
 * @param {Buffer|string} secret - Secret key for signing
 * @returns {string} - Generated JWT
 */
function generateJWT(payload, secret) {
    // Header
    const header = {
        alg: 'HS256', // HMAC-SHA256
        typ: 'JWT'
    };

    // Encode Header and Payload as Base64URL
    const encodedHeader = Buffer.from(JSON.stringify(header)).toString('base64url');
    const encodedPayload = Buffer.from(JSON.stringify(payload)).toString('base64url');

    // Generate signature
    const dataToSign = `${encodedHeader}.${encodedPayload}`;
    const signature = createHmac('sha256', secret).update(dataToSign).digest('base64url');

    // Assemble JWT
    return `${dataToSign}.${signature}`;
}

/**
 * Verifies a JWT and returns its payload if valid
 * @param {string} token - JWT to verify
 * @param {Buffer|string} secret - Secret key for verification
 * @returns {Object} - JWT payload
 * @throws {Error} - If token is invalid or expired
 */
function verifyJWT(token, secret) {
    const parts = token.split('.');

    // 1. Check that the token has exactly 3 parts
    if (parts.length !== 3) {
        throw new Error('Ungültiges JWT-Format');
    }

    const [encodedHeader, encodedPayload, signature] = parts;

    let payload;
    try {
        // 2. Decode payload
        payload = JSON.parse(Buffer.from(encodedPayload, 'base64url').toString());
    } catch (e) {
        throw new Error('Ungültiges Payload-Format');
    }

    // 3. Check expiration date (if present)
    const currentTime = Math.floor(Date.now() / 1000); // Current time in seconds
    if (payload.e && currentTime > payload.e) {
        throw new Error('JWT abgelaufen');
    }

    // 4. Generate new signature and verify (only if not expired)
    const dataToVerify = `${encodedHeader}.${encodedPayload}`;
    const expectedSignature = createHmac('sha256', secret)
        .update(dataToVerify)
        .digest('base64url');

    if (expectedSignature !== signature) {
        throw new Error('Ungültige Signatur');
    }

    // 5. Return payload if all checks pass
    return payload;
}

/**
 * Generates a CRC check value for a given input
 * @param {string|number} value - Input value
 * @returns {string} - CRC check value
 */
function betrugerCrc(value) {
    const temp = crc32.unsigned(value.toString()) & 1023;
    return Buffer.from([base32table[temp >> 5], base32table[temp & 31]]).toString();
}

/**
 * Creates a secret key for JWT operations
 * @param {string} jwtSecret - JWT secret in hex format
 * @returns {Buffer} - Secret key object
 */
function createSecretJwtKey(jwtSecret) {
    return createSecretKey(jwtSecret, 'hex');
}

/**
 * Creates an encryption key
 * @param {string} encKey - Encryption key in hex format
 * @returns {Buffer} - Encryption key object
 */
function createEncryptionKey(encKey) {
    return createSecretKey(encKey, 'hex');
}

module.exports = {
    base32table,
    base32backHash,
    betrugerTimeIvBase32,
    betrugerToBase32,
    betrugerToHex,
    betrugerUrlEncrypt,
    betrugerUrlDecrypt,
    generateJWT,
    verifyJWT,
    betrugerCrc,
    createSecretJwtKey,
    createEncryptionKey
};