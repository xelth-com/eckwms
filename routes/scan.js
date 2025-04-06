// routes/scan.js
// Полный файл маршрута API для обработки сканирований с мобильного приложения
const express = require('express');
const router = express.Router();
const { processScan } = require('../utils/scanHandler');
const { verifyJWT } = require('../utils/encryption');

// Middleware для проверки аутентификации (опционально)
const authenticateDevice = (req, res, next) => {
    try {
        // Проверка наличия токена в заголовке (можно сделать опциональным на начальном этапе)
        const token = req.headers.authorization?.split(' ')[1];
        
        if (token) {
            req.user = verifyJWT(token, global.secretJwt);
        }
        
        // Продолжаем выполнение даже без токена для обратной совместимости
        next();
    } catch (error) {
        // Логируем ошибку, но все равно продолжаем для совместимости с устройствами без аутентификации
        console.warn('Authentication error:', error.message);
        next();
    }
};

// Маршрут для обработки сканирования
router.post('/process', authenticateDevice, async (req, res) => {
    try {
        const { barcode, deviceId } = req.body;
        
        if (!barcode) {
            return res.status(400).json({ 
                success: false, 
                message: 'No barcode provided' 
            });
        }
        
        console.log(`Scan received from device ${deviceId || 'unknown'}: ${barcode}`);
        
        // Обработка штрих-кода с использованием существующей логики
        const result = await processScan(barcode, req.user);
        
        res.json({
            success: true,
            data: result
        });
    } catch (error) {
        console.error('Error processing scan:', error);
        res.status(500).json({
            success: false,
            message: error.message || 'Error processing scan'
        });
    }
});

// Получение информации о последних сканированиях (опционально)
router.get('/recent', authenticateDevice, (req, res) => {
    try {
        // Можно реализовать сохранение истории сканирований и возврат последних записей
        // Это опциональная функциональность
        res.json({
            success: true,
            data: []  // Пустой массив на данном этапе
        });
    } catch (error) {
        res.status(500).json({
            success: false,
            message: error.message || 'Error fetching recent scans'
        });
    }
});

module.exports = router;