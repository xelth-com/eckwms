const crypto = require('crypto');
const { Buffer } = require('buffer');

// --- УТИЛИТАРНЫЕ ФУНКЦИИ (без изменений) ---

const base32table = Buffer.from('0123456789ABCDEFGHJKLMNPQRTUVWXY');
const base32backHash = [];
base32table.forEach((element, index) => {
    base32backHash[element] = index;
});

const betrugerToBase32 = (bufIn) => {
    const iterat = Math.floor(bufIn.length / 5);
    const bufOut = Buffer.alloc(iterat * 8); // Используем alloc вместо allocUnsafe
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

const betrugerToHex = (bufIn) => {
    const iterat = Math.floor(bufIn.length / 8);
    const bufOut = Buffer.alloc(iterat * 5); // Используем alloc вместо allocUnsafe
    for (let i = 0; i < iterat; i++) {
        bufOut[i * 5 + 0] = (base32backHash[bufIn[i * 8 + 0]] << 3) + (base32backHash[bufIn[i * 8 + 1]] >> 2);
        bufOut[i * 5 + 1] = (base32backHash[bufIn[i * 8 + 1]] << 6) + (base32backHash[bufIn[i * 8 + 2]] << 1) + (base32backHash[bufIn[i * 8 + 3]] >> 4);
        bufOut[i * 5 + 2] = (base32backHash[bufIn[i * 8 + 3]] << 4) + (base32backHash[bufIn[i * 8 + 4]] >> 1);
        bufOut[i * 5 + 3] = (base32backHash[bufIn[i * 8 + 4]] << 7) + (base32backHash[bufIn[i * 8 + 5]] << 2) + (base32backHash[bufIn[i * 8 + 6]] >> 3);
        bufOut[i * 5 + 4] = (base32backHash[bufIn[i * 8 + 6]] << 5) + (base32backHash[bufIn[i * 8 + 7]]);
    }
    return bufOut;
};


// --- КОНФИГУРАЦИЯ ПРОТОКОЛА ---

const encKey = crypto.createSecretKey('2f8cffbfb357cb957a427fc6669d6f92100fdd471d1ed2d2', 'hex');
const algorithm = 'aes-192-gcm';


// --- ОСНОВНЫЕ ФУНКЦИИ МОДУЛЯ ---

/**
 * Encrypts a 19-byte payload into a secure, compact URL.
 * @param {Buffer} payloadBuffer19 - The 19-byte binary payload to encrypt.
 * @param {string} userId - User ID for context derivation (e.g., "M3A").
 * @param {string} domain - Domain for context derivation (e.g., "ECK1.COM").
 * @returns {string} - The complete, encrypted URL.
 */
const betrugerUrlEncrypt = (payloadBuffer19, userId, domain) => {
    if (!Buffer.isBuffer(payloadBuffer19) || payloadBuffer19.length !== 19) {
        throw new Error('Payload must be a 19-byte Buffer.');
    }

    // 1. Вычисляем контекстную часть IV (3 байта)
    const contextPart = crypto.createHash('sha256')
        .update(userId + domain)
        .digest()
        .subarray(0, 3);

    // 2. Генерируем уникальную часть IV - Nonce (9 байт)
    const noncePart = crypto.randomBytes(9);

    // 3. Собираем полный 12-байтовый IV
    const fullIv = Buffer.concat([contextPart, noncePart]);

    // 4. Инициализируем шифратор
    const cipher = crypto.createCipheriv(algorithm, encKey, fullIv);

    // 5. Шифруем 19-байтный payload
    const ciphertext = Buffer.concat([
        cipher.update(payloadBuffer19),
        cipher.final()
    ]);

    // 6. Получаем тег и урезаем его до 12 байт
    const fullTag = cipher.getAuthTag();
    const truncatedTag = fullTag.subarray(0, 12);

    // 7. Собираем 40-байтовый пакет: [9-байт Nonce] + [19-байт Шифротекст] + [12-байт Тег]
    const binaryPacket = Buffer.concat([noncePart, ciphertext, truncatedTag]);

    // 8. Кодируем пакет в 64-символьный Base32 блок
    const base32Block = betrugerToBase32(binaryPacket).toString();

    // 9. Возвращаем финальный URL
    return `${domain}/${base32Block}${userId}`;
};

/**
 * Decrypts a secure URL back to the original 19-byte payload.
 * @param {string} betrugerUrl - The complete encrypted URL.
 * @returns {Buffer|false} - The original 19-byte payload as a Buffer, or false on failure.
 */
const betrugerUrlDecrypt = (betrugerUrl) => {
    try {
        // 1. Разбираем URL
        const urlParts = betrugerUrl.split('/');
        if (urlParts.length !== 2) throw new Error('Invalid URL format');
        
        const domain = urlParts[0];
        const cryptoBlockAndUserId = urlParts[1];
        
        if (cryptoBlockAndUserId.length < 64) throw new Error('URL crypto block is too short');
        
        const cryptoBlock = cryptoBlockAndUserId.substring(0, 64);
        const userId = cryptoBlockAndUserId.substring(64);

        if (!userId) throw new Error('User ID is missing from URL');

        // 2. Декодируем крипто-блок в 40-байтовый пакет
        const binaryPacket = betrugerToHex(Buffer.from(cryptoBlock));
        if (binaryPacket.length !== 40) throw new Error('Decoded packet length is not 40 bytes');

        // 3. Разбираем пакет на компоненты
        const noncePart = binaryPacket.subarray(0, 9);       // 9 байт
        const ciphertext = binaryPacket.subarray(9, 28);     // 19 байт
        const truncatedTag = binaryPacket.subarray(28, 40);  // 12 байт

        // 4. Восстанавливаем полный 12-байтовый IV
        const contextPart = crypto.createHash('sha256')
            .update(userId + domain)
            .digest()
            .subarray(0, 3);
        const fullIv = Buffer.concat([contextPart, noncePart]);

        // 5. Инициализируем дешифратор
        const decipher = crypto.createDecipheriv(algorithm, encKey, fullIv);
        
        // 6. Устанавливаем полученный урезанный тег
        decipher.setAuthTag(truncatedTag);

        // 7. Расшифровываем
        const decryptedBuffer = Buffer.concat([
            decipher.update(ciphertext),
            decipher.final() // Эта строка вызовет ошибку, если тег не совпадет
        ]);

        return decryptedBuffer;

    } catch (err) {
        if (err.message.includes('Unsupported state')) {
            console.error('Decryption failed: Authentication tag mismatch.');
        } else {
            console.error('Decryption error:', err.message);
        }
        return false;
    }
};


// --- ПРИМЕР ИСПОЛЬЗОВАНИЯ И ТЕСТИРОВАНИЕ ---

if (require.main === module) {
    console.log("--- Running Test Case ---");

    // Создаем 19-байтный payload согласно вашей идее "1 байт префикс + 18 байт данные"
    const testPayload = Buffer.alloc(19);
    testPayload[0] = 0x01; // Префикс, означающий "это простой текстовый ID"
    Buffer.from('i453Sfdfke48').copy(testPayload, 1); // Копируем ID в оставшееся место

    const testUserId = 'M3A';
    const testDomain = 'ECK1.COM';

    console.log('Original Payload (hex):', testPayload.toString('hex'));
    
    // Тест шифрования
    const encryptedUrl = betrugerUrlEncrypt(testPayload, testUserId, testDomain);
    console.log('Encrypted URL:', encryptedUrl);
    console.log('URL Length:', encryptedUrl.length);

    // Тест расшифровки
    const decryptedPayload = betrugerUrlDecrypt(encryptedUrl);

    if (decryptedPayload) {
        console.log('Decrypted Payload (hex):', decryptedPayload.toString('hex'));
        console.log('Payloads Match:', Buffer.compare(testPayload, decryptedPayload) === 0);
    } else {
        console.log("Decryption failed as expected for an invalid input or during testing.");
    }
}


module.exports = {
    betrugerUrlEncrypt,
    betrugerUrlDecrypt
};