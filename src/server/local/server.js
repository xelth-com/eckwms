// Main Express application for eckwms
require('dotenv').config(); // Убедись, что это вызывается очень рано

const express = require('express');
const path = require('path');
const fs = require('fs');
const { resolve } = require('path');
const app = express();
const bodyParser = require('body-parser');
const helmet = require('helmet');
const { createSecretJwtKey } = require('../../shared/utils/encryption'); // Убедись, что это используется правильно или удали, если ключи создаются внутри utils/encryption.js
const { appendFile } = require('fs/promises');
// Убрал 'express-session' и 'connect-pg-simple' т.к. JWT используется для auth API
const passport = require('passport');
const initI18n = require('./middleware/i18n');
const { translationQueue } = require('./middleware/i18n'); // Убедись, что используется
const createLanguageMiddleware = require('./middleware/languageMiddleware');
const { requireAdmin } = require('./middleware/auth'); // Middleware auth
const i18next = require('i18next');
const createHtmlTranslationInterceptor = require('./middleware/htmlTranslationInterceptor');
const { createProxyMiddleware } = require('http-proxy-middleware'); // <-- ДОБАВЛЕНО для NodeBB прокси
const { collectAndReportDiagnostics } = require('./utils/startupDiagnostics');

// Import routes
const apiRoutes = require('./routes/api');
const rmaRoutes = require('./routes/rma');
const statusRoutes = require('./routes/status');
const adminRoutes = require('./routes/admin');
const authRoutes = require('./routes/auth');
const translationApiRoutes = require('./routes/translation-api');
const translationAdminRoutes = require('./routes/translation-admin');
const eckwmsRoutes = require('./routes/eckwms');
// Убрал mavenProxyRoutes, так как сказали, что он нерелевантен для проксирования сайта
// const mavenProxyRoutes = require('./routes/mavenProxy'); // Если все же нужен, верни

// NEW: Import scan routes
const scanRoutes = require('./routes/scan');

// NEW: Import upload routes
const uploadRoutes = require('./routes/upload');

// NEW: Import setup routes for device pairing
const setupRoutes = require('./routes/setup');

// Import middleware
const { errorHandler, requestLogger } = require('./middleware');

// Import models (Legacy - возможно, часть уже не нужна при переходе на PG)
const { Betruger, User, Order, Place, Box, Item, Dict } = require('../../shared/models');
const { writeLargeMapToFile } = require('./utils/fileUtils');

// Import PostgreSQL models
const db = require('../../shared/models/postgresql');

// Global variables (Подумай, можно ли избавиться от части глобальных переменных, особенно для данных, которые теперь в PG)
// global.dict = new Dict(''); // Вероятно, можно заменить на работу с PG
// global.dicts = new Map(); // Вероятно, можно заменить на работу с PG
global.user = new User(''); // Используется ли еще старая модель? UserAuth из PG теперь основная.
// ... и так далее для других глобальных переменных ...
global.users = new Map();
global.orders = new Map();
global.uppers = new Map();
global.classes = new Map();
global.items = new Map();
global.boxes = new Map();
global.places = new Map();
global.runOnServer = Object.hasOwn(process.env, 'pm_id');
global.baseDirectory = __dirname + '/';

// Инициализация JWT секрета глобально (ВАЖНО!)
// Убедись, что JWT_SECRET в .env ЕСТЬ и он тот же, что используется в utils/encryption.js
if (!process.env.JWT_SECRET) {
    throw new Error("Критическая ошибка: JWT_SECRET не найден в .env!");
}
global.secretJwt = process.env.JWT_SECRET; // Используется в middleware/auth.js для jwt.sign/verify

// Serial numbers for items/boxes/places (Legacy - возможно, можно перенести в базу)
global.serialIi = 999999999999999;
global.serialI = 1;
global.serialB = 1;
global.serialP = 1;

// --- Логгирование запроса (оставлено) ---
app.use((req, res, next) => {
    console.log('============================= Request Start Point =========================================');
    next();
});

// --- Безопасность ---
app.use(helmet({
    contentSecurityPolicy: false, // Подумай, можно ли включить и настроить CSP
    crossOriginEmbedderPolicy: false // Возможно, тоже стоит настроить
}));

// --- Основные Middleware ---
app.use(express.json()); // Для JSON тел запросов
app.use(express.urlencoded({ extended: true })); // Для URL-encoded тел запросов
app.use(bodyParser.text({ type: 'text/html' })); // Для text/html тел запросов (используется?)

app.use(requestLogger); // Логгер запросов

// --- Инициализация Passport (для JWT стратегии) ---
const configPassport = require('./config/passport');
configPassport(passport); // Передаем экземпляр passport для конфигурации стратегии 'jwt'
app.use(passport.initialize()); // Passport инициализация

// --- Middleware для языка и i18n ---
app.use(initI18n()); // i18n инициализация
app.use(createHtmlTranslationInterceptor(i18next)); // Перехватчик HTML для переводов
app.use(createLanguageMiddleware()); // Установка языка в заголовки

// --- Middleware для контроля кеширования (оставлено) ---
app.use((req, res, next) => {
    res.set({
      'Cache-Control': 'no-store, no-cache, must-revalidate, private, max-age=0',
      'Pragma': 'no-cache',
      'Expires': 0,
      'Surrogate-Control': 'no-store',
      'ETag': `W/"${Date.now().toString()}"`,
      'Last-Modified': new Date().toUTCString()
    });
    next();
  });

// --- Статические файлы (после middleware кеширования) ---
app.use(express.static(path.join(__dirname, '../../../public'), {
    index: false,
    etag: false,
    lastModified: false,
    maxAge: 0,
    cacheControl: false
}));

// --- ПРОКСИРОВАНИЕ ДЛЯ NODEBB ---
const nodebbTarget = process.env.NODEBB_URL || 'http://localhost:4567'; // URL вашего NodeBB из .env или по умолчанию
const nodebbProxyOptions = {
  target: nodebbTarget,
  changeOrigin: true, // Нужно для корректной работы cookies/заголовков между доменами/портами
  ws: true,           // ВАЖНО: Включает проксирование WebSocket для чата и уведомлений NodeBB
  pathRewrite: {
    '^/forum': '',    // Убираем '/forum' из URL перед отправкой в NodeBB
                     // (NodeBB будет думать, что работает в корне '/')
                     // Если вы настроите NodeBB на работу с base_url '/forum', это можно убрать.
  },
  logLevel: process.env.NODE_ENV === 'development' ? 'debug' : 'info', // Логирование для отладки
  onError: (err, req, res) => { // Обработка ошибок прокси
    console.error('[NodeBB Proxy] Ошибка прокси:', err);
    if (res && !res.headersSent) {
        res.writeHead(502, { 'Content-Type': 'text/plain' });
    }
    if (res && !res.writableEnded) {
      res.end('Proxy Error: Could not connect to NodeBB service.');
    }
  }
};

// Применяем прокси ко всем запросам, начинающимся с /forum
app.use('/forum', createProxyMiddleware(nodebbProxyOptions));
console.log(`[NodeBB Proxy] Настроено проксирование /forum -> ${nodebbTarget}`);
// --- КОНЕЦ ПРОКСИРОВАНИЯ NODEBB ---

// --- Маршруты приложения ---
app.use('/api', apiRoutes);
app.use('/rma', rmaRoutes);
app.use('/status', statusRoutes);
app.use('/admin', adminRoutes); // Возможно, стоит защитить через requireAdmin глобально здесь?
app.use('/auth', authRoutes);
app.use('/api', translationApiRoutes); // Дублируется '/api', убедись, что нет конфликтов
app.use('/translation-admin', requireAdmin, translationAdminRoutes); // Защищено админкой
app.use('/eckwms', eckwmsRoutes);
app.use('/eckwms/api/upload', uploadRoutes);
app.use('/api/internal', setupRoutes); // Device pairing endpoints
// app.use('/nexus', mavenProxyRoutes); // Если нужен Maven Proxy, верни

// Legacy scan routes removed - use /eckwms/api/scan instead with the new intelligent buffer architecture
// app.use('/api/scan', scanRoutes);

// --- Основной маршрут для SPA ---
app.get('/', (req, res) => {
    res.sendFile(path.join(__dirname, 'html', 'index.html'));
});


// Health check endpoint for client connectivity testing
app.get('/health', (req, res) => {
  res.status(200).json({ status: 'ok', server: 'local' });
});
// --- Маршрут для JWT верификации (пример, возможно, не нужен) ---
// Если используется только для отладки, можно удалить или защитить.
// Сейчас он использует глобальные переменные, что не идеально.
app.get('/jwt/:token', (req, res) => {
    const { token } = req.params;
    res.status(200);
    res.setHeader('Content-Type', 'text/html');
    try {
        // Используй verifyJWT из твоего модуля encryption, а не из временного require
        const { verifyJWT } = require('../../shared/utils/encryption'); // Предполагается, что она экспортирована
        const payload = verifyJWT(token); // Используем секрет, заданный глобально

        if (!payload) {
           return res.send('Invalid or expired token.');
        }

        // Логика ниже сильно зависит от старых глобальных переменных (orders)
        // Ее нужно адаптировать или удалить, если она больше не нужна
        const { prettyPrintObject, maskObjectFields } = require('./utils/formatUtils');
        const fieldsToMask = ["comp", "pers", "str", "cem", "iem"]; // Старые поля?
        // Пример: попробуем найти пользователя по ID из токена
        // const user = await db.UserAuth.findByPk(payload.userId); // Нужен async
        // const maskedObj = user ? maskObjectFields(user.toJSON(), fieldsToMask) : { message: "User not found or legacy data missing" };

        // Заглушка, пока не ясно, что должно отображаться
         const maskedObj = payload; // Пока просто покажем payload

        res.send('<div style="width: min-content;">' + prettyPrintObject(maskedObj) + '</div><br><br><a href="https://m3.repair/" style="color:#1e2071;">M3 Mobile GmbH Homepage</a>');
    } catch (error) {
        console.error("JWT verification route error:", error);
        res.send(' Nice try, but an error occurred.');
    }
});

// --- Маршрут для админки переводов (защищенный) ---
app.get('/admin/translations', requireAdmin, (req, res) => {
    res.sendFile(path.join(__dirname, 'html/admin/translations.html'));
});

// --- Основной POST обработчик (Legacy? Используется ли еще?) ---
// Этот обработчик выглядит как часть старой системы. Если он не нужен, удали.
// Если нужен, нужно его адаптировать под новую структуру данных.
app.post('/', async (req, res) => {
    try {
        const parsedData = req.body;

        if (parsedData.dest === 'csv') {
            console.warn("CSV export endpoint '/' triggered. Uses legacy global data.");
            const csvData = await generateCsvData(); // Использует глобальные 'boxes'
            res.writeHead(200, {
                'Content-Type': 'text/csv',
                'Content-Disposition': 'attachment; filename="export.csv"',
                'Content-Length': Buffer.byteLength(csvData) // Используй Buffer.byteLength для корректной длины
            });
            res.end(csvData);
        } else {
            console.warn("HTML parsing endpoint '/' triggered. Uses legacy parser.");
            const { parseHtml } = require('./utils/htmlParser'); // Старый парсер?
            const htmlContent = await parseHtml(parsedData); // Использует старую логику?
            res.status(200).send(htmlContent);
        }
    } catch (error) {
        console.error("Error in main POST handler:", error);
        res.status(500).send('Server error: ' + error.message);
    }
});

// --- Internal API endpoint for global server ---
// This endpoint provides public-safe data for items/boxes/orders
app.get('/api/internal/public-data/:id', (req, res) => {
    const { id } = req.params;

    console.log(`[Local Server] Internal API request for ID: ${id}`);

    try {
        // Check if ID exists in any of the legacy global maps
        let data = null;
        let type = null;

        // Check items
        if (global.items && global.items.has(id)) {
            const item = global.items.get(id);
            type = 'item';
            data = {
                id: id,
                type: type,
                model: item.mod || 'Unknown',
                status: item.status || 'Unknown',
                timestamp: item.timestamp || new Date().toISOString()
            };
        }
        // Check boxes
        else if (global.boxes && global.boxes.has(id)) {
            const box = global.boxes.get(id);
            type = 'box';
            data = {
                id: id,
                type: type,
                status: box.status || 'Unknown',
                timestamp: box.timestamp || new Date().toISOString()
            };
        }
        // Check orders
        else if (global.orders && global.orders.has(id)) {
            const order = global.orders.get(id);
            type = 'order';
            data = {
                id: id,
                type: type,
                status: order.status || 'Unknown',
                timestamp: order.timestamp || new Date().toISOString()
            };
        }

        if (data) {
            console.log(`[Local Server] Found ${type} with ID: ${id}`);
            return res.json(data);
        } else {
            console.log(`[Local Server] No data found for ID: ${id}`);
            return res.status(404).json({
                error: 'Item not found',
                message: 'No information available for this code.'
            });
        }
    } catch (error) {
        console.error('[Local Server] Error in internal API:', error);
        return res.status(500).json({
            error: 'Internal server error',
            message: 'An error occurred while retrieving information.'
        });
    }
});

// --- Обработчик ошибок (должен быть последним) ---
app.use(errorHandler);

// --- Вспомогательные функции (Legacy) ---
// Логгирование в файл (оставлено, но можно рассмотреть более современные логгеры)
async function writeLog(str) {
    const dateTemp = new Date(); // Используем локальное время? Или UTC?
    const logDir = resolve('./logs');
    if (!fs.existsSync(logDir)) {
        fs.mkdirSync(logDir, { recursive: true });
    }
    // Формат имени файла ГГММ.txt
    const filename = `${dateTemp.getFullYear().toString().slice(-2)}${('0' + (dateTemp.getMonth() + 1)).slice(-2)}.txt`;
    const timestamp = `${dateTemp.getDate()}_${dateTemp.getHours()}:${dateTemp.getMinutes()}:${dateTemp.getSeconds()}`;
    const memoryUsage = Object.values(process.memoryUsage()).map(v => `${(v / 1024 / 1024).toFixed(2)}MB`).join(', ');
    return appendFile(resolve(logDir, filename), `${str}\tMemory: ${memoryUsage}\tTimestamp: ${timestamp}\n`);
}

// Генерация CSV (использует глобальные переменные - legacy)
async function generateCsvData() {
    let csv = 'SN /PN;Model;IN DATE;Out Date;Customer;SKU;email;Address;Zip Code;City;Complaint;Verification;Cause;Result;Shipping;Invoice number;Special note; warranty;condition;Used New Parts;Used Refurbished Parts\n';
    console.warn("generateCsvData relies on global 'boxes'. Data might be stale or incomplete.");
    // Логика генерации CSV сильно зависит от структуры глобальных 'boxes'
    // Нужно переписать для работы с данными из PostgreSQL
    boxes.forEach((element) => {
        let packIn = false;
        let packOut = false;
        element.loc?.forEach((locElement) => {
            if (locElement[0] == 'p000000000000000030') packIn = locElement[1];
            if (locElement[0] == 'p000000000000000060') packOut = locElement[1];
        });

        if (packIn || packOut) {
            // TODO: Implement CSV generation logic based on element structure
          // This needs to be adapted to the new data source (PostgreSQL)
        }
    });
    return csv;
}

// --- Инициализация приложения ---
async function initialize() {
    // Убрал legacy dataInit функции, если данные теперь в PG
    // const { initialisation, classesUpdate, upperUpdate } = require('./utils/dataInit');

    try {
        // 1. Инициализация PostgreSQL
        await db.sequelize.authenticate();
        console.log('PostgreSQL connection established successfully.');

        // Синхронизация моделей (только в development)
        if (process.env.NODE_ENV === 'development') {
            const alterDb = process.env.DB_ALTER === 'true';
            console.log(`Syncing PostgreSQL models (alter: ${alterDb})...`);
            await db.sequelize.sync({ alter: alterDb });
            console.log('PostgreSQL models synchronized.');
        }

        // 2. Инициализация legacy данных (если все еще нужны)
        // Подумай, нужно ли это еще, если данные перенесены в PG
        if (fs.existsSync(resolve(baseDirectory, 'base/users.json'))) { // Пример проверки
             console.log('Attempting to initialize legacy data from JSON files...');
             // await initialisation(baseDirectory); // Вызов старой инициализации
             console.log('Legacy data initialization skipped or completed.');
        } else {
             console.log('Legacy JSON files not found, skipping legacy initialization.');
        }


        await writeLog('Server startup initiated.'); // Лог запуска

        // Убрал classesUpdate() и upperUpdate(), если это часть legacy
        // classesUpdate();
        // upperUpdate();

        // 3. Настройка Graceful Shutdown
        process.on('SIGINT', async () => {
            console.log('\nReceived SIGINT. Shutting down gracefully...');
            await logOut(baseDirectory); // Сохранение legacy данных (если нужно)
            await db.sequelize.close(); // Закрытие соединения с PG
            console.log('PostgreSQL connection closed.');
            process.exit(0);
        });
        process.on('SIGTERM', async () => { // Также обрабатываем SIGTERM
            console.log('\nReceived SIGTERM. Shutting down gracefully...');
            await logOut(baseDirectory);
            await db.sequelize.close();
            console.log('PostgreSQL connection closed.');
            process.exit(0);
        });

        // 4. Запуск сервера
        const PORT = process.env.LOCAL_SERVER_PORT || process.env.PORT || 3100;
        app.listen(PORT, () => {
            console.log(`eckwms server running on port ${PORT} in ${process.env.NODE_ENV} mode.`);
            writeLog('Server started successfully.'); // Лог успешного запуска

            // Report diagnostics to global server on startup
            if (process.env.NODE_ENV !== 'development-no-sync') { // Add a flag to disable sync for simple testing
                collectAndReportDiagnostics();
            }
        });

    } catch (err) {
        console.error('FATAL ERROR: Failed to initialize application:', err);
        writeLog(`FATAL ERROR during initialization: ${err.message || err}`);
        process.exit(1); // Выход при фатальной ошибке инициализации
    }
}

// --- Функция сохранения Legacy данных при выходе (если нужно) ---
async function logOut(mainDirectory) {
    console.log('Saving legacy data to JSON files (if applicable)...');
    const basePath = resolve(mainDirectory, 'base');
    try {
        // Проверяем существование папки перед записью
        if (!fs.existsSync(basePath)) {
            console.warn(`Directory ${basePath} not found. Skipping legacy data save.`);
            return;
        }
        // Оборачиваем каждую запись в try/catch, чтобы одна ошибка не прервала все
        try { await writeLargeMapToFile(users, resolve(basePath, 'users.json')); } catch(e) { console.error('Error saving users.json:', e); }
        try { await writeLargeMapToFile(orders, resolve(basePath, 'orders.json')); } catch(e) { console.error('Error saving orders.json:', e); }
        try { await writeLargeMapToFile(items, resolve(basePath, 'items.json')); } catch(e) { console.error('Error saving items.json:', e); }
        try { await writeLargeMapToFile(boxes, resolve(basePath, 'boxes.json')); } catch(e) { console.error('Error saving boxes.json:', e); }
        try { await writeLargeMapToFile(places, resolve(basePath, 'places.json')); } catch(e) { console.error('Error saving places.json:', e); }
        try { await writeLargeMapToFile(classes, resolve(basePath, 'classes.json')); } catch(e) { console.error('Error saving classes.json:', e); }
        try { await writeLargeMapToFile(uppers, resolve(basePath, 'uppers.json')); } catch(e) { console.error('Error saving uppers.json:', e); }
        try { await writeLargeMapToFile(dicts, resolve(basePath, 'dicts.json')); } catch(e) { console.error('Error saving dicts.json:', e); }
        try { await fs.promises.writeFile(resolve(basePath, 'ini.json'), JSON.stringify({ serialIi, serialI, serialB, serialP })); } catch(e) { console.error('Error saving ini.json:', e); }

        await writeLog('Server shutdown completed.'); // Лог завершения
        console.log('Legacy data saved.');
    } catch (err) {
        console.error('Error during logOut (saving legacy data):', err);
        writeLog(`Error during logOut: ${err.message || err}`);
    }
}

// --- Запуск инициализации ---
initialize();

module.exports = app; // Экспорт для тестов или других нужд