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

// --- Инициализация ключей и конфигурации на уровне модуля ---

// ВАЖНО: require('dotenv').config() должен быть вызван РАНЬШЕ в главном файле приложения (app.js)!
// Этот модуль просто ожидает, что process.env уже настроен.

// Загружаем шестнадцатеричные строки ключей из переменных окружения
const encKeyHex = process.env.ENC_KEY;
const jwtSecretHex = process.env.JWT_SECRET;

// Проверяем наличие ключей
if (!encKeyHex) {
    throw new Error('КРИТИЧЕСКАЯ ОШИБКА: Переменная окружения ENC_KEY не установлена! Модуль шифрования не может быть инициализирован.');
}
if (!jwtSecretHex) {
    // Можно сделать это предупреждением, если JWT используется не всегда
    // console.warn('Предупреждение: Переменная окружения JWT_SECRET не установлена.');
    // Или ошибкой, если JWT нужен всегда:
    throw new Error('КРИТИЧЕСКАЯ ОШИБКА: Переменная окружения JWT_SECRET не установлена! Модуль шифрования не может быть инициализирован.');
}

// Создаем объекты KeyObject ОДИН РАЗ при загрузке модуля
// Используем функции, которые были в вашем файле - они корректно используют createSecretKey(..., 'hex')
const internalEncKey = createSecretKey(encKeyHex, 'hex');
const internalJwtKey = createSecretKey(jwtSecretHex, 'hex');


const algorithm = 'aes-192-gcm';

console.log('Модуль шифрования инициализирован с алгоритмом:', algorithm); // Лог для подтверждения

// --- Конец инициализации ключей ---

// --- Утилиты Base32 (без изменений) ---
const base32table = Buffer.from('0123456789ABCDEFGHJKLMNPQRTUVWXY');
const base32backHash = [];
base32table.forEach((element, index) => {
    base32backHash[element] = index;
});

/**
 * Generates a time-based IV using Base32 encoding
 * @param {number} ivLength - Length of the IV to generate
 * @returns {Buffer} - Generated IV
 */
const betrugerTimeIvBase32 = (ivLength) => {
    if (ivLength < 3) ivLength = 3;
    const tempTime = Math.floor(Date.now() / 7777777); // Consider stability of this division factor
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
 * Converts Base32 encoded buffer back to original format (likely bytes/hex)
 * @param {Buffer} bufIn - Base32 encoded buffer
 * @returns {Buffer} - Decoded buffer
 */
const betrugerToHex = (bufIn) => { // Note: Function name might be misleading if output isn't hex
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
// --- Конец утилит Base32 ---


// --- Функции шифрования/дешифрования (используют внутренний ключ) ---

/**
 * Encrypts a string and formats it for use in a URL.
 * Uses the encryption key initialized at module load.
 * @param {string} string19 - String to encrypt (expected length 19?)
 * @returns {string|boolean} - Encrypted URL-safe string, or false on error.
 */
const betrugerUrlEncrypt = (string19) => {
    try {
        const encryptionKey = internalEncKey; // Используем ключ модуля
        const betIv = betrugerTimeIvBase32(9); // Generate Base32 IV part
        // Create the full IV for AES-GCM (needs to be unique per encryption with the same key)
        // The original code concatenates the 9-byte Base32 IV with itself.
        // This results in an 18-byte IV, which is non-standard for GCM (usually 12 bytes).
        // While it might work, using a standard 12-byte random IV is generally recommended for GCM.
        // Let's stick to the original logic for now, but be aware it's unusual.
        const iv = Buffer.concat([betIv, betIv], 16); // Resulting IV length is 18 bytes? Check crypto requirements. AES-GCM often prefers 12 bytes.

        const cipher = createCipheriv(algorithm, encryptionKey, iv);

        let encrypted = cipher.update(string19, 'utf8', 'hex');
        encrypted += cipher.final('hex');
        const authTag = cipher.getAuthTag(); // Get the authentication tag (16 bytes for AES-GCM)

        // Combine encrypted data and authTag, convert to Base32, append Base32 IV part
        const dataToEncode = Buffer.concat([Buffer.from(encrypted, 'hex'), authTag]);
        const base32EncodedData = betrugerToBase32(dataToEncode);

        // Combine Base32 data and Base32 IV
        return Buffer.concat([base32EncodedData, betIv]).toString(); // Result length depends on input and Base32 padding

    } catch (error) {
        console.error('Error during encryption:', error);
        return false;
    }
};

/**
 * Decrypts a URL-formatted encrypted string.
 * Uses the decryption key initialized at module load.
 * @param {string} betrugerUrl - Encrypted URL to decrypt
 * @returns {string|boolean} - Decrypted string or false if decryption fails/format is invalid
 */
const betrugerUrlDecrypt = (betrugerUrl) => {
    try {
        const decryptionKey = internalEncKey; // Используем ключ модуля

        // Basic format checks (adjust lengths based on actual expected format)
        // Original code checks for length 76. Let's keep it for now.
        if (typeof betrugerUrl !== 'string' || betrugerUrl.length !== 76 ||
            betrugerUrl.substring(74, 76) !== 'M3' || // What is 'M3'? A marker?
            !betrugerUrl.substring(0, 9).match(/^ECK[123]\.COM\//)) { // Use ^ for start anchor
            // console.warn('Invalid betrugerUrl format or prefix/suffix mismatch.');
            return false;
        }

        // Extract Base32 IV (last 9 chars) and Base32 data (chars 9 to 65)
        const betIv = Buffer.from(betrugerUrl.substring(65, 74));
        const base32Data = Buffer.from(betrugerUrl.substring(9, 65));

        // Decode the main data part from Base32
        const decodedData = betrugerToHex(base32Data); // Returns buffer with encrypted message + auth tag

        // Separate encrypted message and authentication tag
        // IMPORTANT: The Auth Tag for AES-GCM is typically 16 bytes (32 hex chars).
        // The original code sliced at 19 and 35 bytes in the *decoded* buffer. This needs verification.
        // Assuming the auth tag is the last 16 bytes of decodedData:
        if (decodedData.length <= 16) {
            console.error('Decoded data too short to contain auth tag.');
            return false;
        }
        const authTagLength = 16; // Standard for AES-GCM
        const recMes = decodedData.subarray(0, decodedData.length - authTagLength); // Encrypted message bytes
        const recATag = decodedData.subarray(decodedData.length - authTagLength); // Auth tag bytes

        // Reconstruct the full IV used for decryption (same unusual logic as encryption)
        const iv = Buffer.concat([betIv, betIv], 16);

        // Create decipher
        const decipher = createDecipheriv(algorithm, decryptionKey, iv);

        // Set the received authentication tag BEFORE decrypting
        decipher.setAuthTag(recATag);

        // Decrypt the message
        let decrypted = decipher.update(recMes); // Input can be buffer
        decrypted = Buffer.concat([decrypted, decipher.final()]); // Get remaining data

        // If final() throws, it means authentication failed (tag mismatch)
        return decrypted.toString('utf8');

    } catch (error) {
        // Log specific errors. Tag mismatch errors often have code 'ERR_CRYPTO_INVALID_AUTH_TAG'
        if (error.code === 'ERR_CRYPTO_INVALID_AUTH_TAG') {
             console.warn('Decryption failed: Invalid authentication tag.');
        } else {
             console.error('Error during decryption:', error);
        }
        return false;
    }
};
// --- Конец функций шифрования/дешифрования ---


// --- Функции JWT (используют внутренний ключ) ---

/**
 * Generates a JWT (JSON Web Token).
 * Uses the JWT secret initialized at module load.
 * @param {Object} payload - JWT payload (should include 'iat' and optionally 'exp')
 * @returns {string|boolean} - Generated JWT or false on error
 */
function generateJWT(payload) {
    try {
        if (!internalJwtKey) {
            throw new Error('JWT Secret не инициализирован');
        }
        // Add issued-at timestamp if not present
        if (!payload.iat) {
            payload.iat = Math.floor(Date.now() / 1000);
        }

        const header = { alg: 'HS256', typ: 'JWT' };
        const encodedHeader = Buffer.from(JSON.stringify(header)).toString('base64url');
        const encodedPayload = Buffer.from(JSON.stringify(payload)).toString('base64url');
        const dataToSign = `${encodedHeader}.${encodedPayload}`;

        // Use the internal module key
        const signature = createHmac('sha256', internalJwtKey)
            .update(dataToSign)
            .digest('base64url');

        return `${dataToSign}.${signature}`;
    } catch(error) {
        console.error("Error generating JWT:", error);
        return false;
    }
}

/**
 * Verifies a JWT and returns its payload if valid.
 * Uses the JWT secret initialized at module load.
 * @param {string} token - JWT to verify
 * @returns {Object|null} - JWT payload if valid and not expired, otherwise null
 */
function verifyJWT(token) {
    try {
        if (!internalJwtKey) {
            throw new Error('JWT Secret не инициализирован');
        }
        if (typeof token !== 'string') {
             return null; // Or throw?
        }

        const parts = token.split('.');
        if (parts.length !== 3) {
            // console.warn('Invalid JWT format (not 3 parts)');
            return null; // Or throw?
        }

        const [encodedHeader, encodedPayload, signature] = parts;

        // Verify signature first
        const dataToVerify = `${encodedHeader}.${encodedPayload}`;
        const expectedSignature = createHmac('sha256', internalJwtKey)
            .update(dataToVerify)
            .digest('base64url');

        if (expectedSignature !== signature) {
            // console.warn('Invalid JWT signature');
            return null; // Or throw?
        }

        // Decode payload *after* signature verification
        const payload = JSON.parse(Buffer.from(encodedPayload, 'base64url').toString());

        // Check expiration date (if present)
        const currentTime = Math.floor(Date.now() / 1000);
        if (payload.exp && currentTime > payload.exp) { // Changed from 'e' to standard 'exp'
            // console.warn('JWT expired');
            return null; // Indicate expiration by returning null
        }
         if (payload.nbf && currentTime < payload.nbf) { // Check not-before time
             // console.warn('JWT not yet valid (nbf)');
            return null;
         }

        return payload; // Return payload if valid and not expired

    } catch (error) {
        // Catch JSON parsing errors, HMAC errors etc.
        // console.error('Error verifying JWT:', error);
        return null; // Indicate verification failure
    }
}
// --- Конец функций JWT ---


/**
 * Generates a CRC check value for a given input
 * @param {string|number} value - Input value
 * @returns {string} - CRC check value (2 Base32 chars)
 */
function betrugerCrc(value) {
    const temp = crc32.unsigned(value.toString()) & 1023; // Mask for 10 bits
    return String.fromCharCode(base32table[temp >> 5]) + String.fromCharCode(base32table[temp & 31]); // More direct conversion
}


// --- Экспорт публичных функций ---
// Функции createSecretJwtKey и createEncryptionKey больше не экспортируются,
// так как ключи создаются внутри модуля.
module.exports = {
    // Утилиты Base32 (если они нужны где-то еще)
    // base32table,
    // base32backHash,
    betrugerTimeIvBase32, // Might not be needed outside if IV generation is internal
    betrugerToBase32,     // Might not be needed outside
    betrugerToHex,        // Might not be needed outside

    // Основные функции
    betrugerUrlEncrypt,
    betrugerUrlDecrypt,
    generateJWT,
    verifyJWT,
    betrugerCrc
};