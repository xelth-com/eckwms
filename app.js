// Main Express application for eckwms
require('dotenv').config();
const express = require('express');
const path = require('path');
const fs = require('fs');
const { resolve } = require('path');
const app = express();
const bodyParser = require('body-parser');
const helmet = require('helmet');
const morgan = require('morgan');
const { createSecretJwtKey } = require('./utils/encryption');
const { appendFile } = require('fs/promises');
const session = require('express-session');
const pgSession = require('connect-pg-simple')(session);
const passport = require('passport');
const initI18n = require('./middleware/i18n');
const { translationQueue } = require('./middleware/i18n');
// Import middleware/auth
const { requireAdmin } = require('./middleware/auth');
const htmlInterceptor = require('./middleware/htmlInterceptor');
const i18next = require('i18next');


// Import routes
const apiRoutes = require('./routes/api');
const rmaRoutes = require('./routes/rma');
const statusRoutes = require('./routes/status');
const adminRoutes = require('./routes/admin');
const authRoutes = require('./routes/auth');
const translationApiRoutes = require('./routes/translation-api');
const translationAdminRoutes = require('./routes/translation-admin');

// Import middleware
const { errorHandler, requestLogger } = require('./middleware');

// Import models
const { Betruger, User, Order, Place, Box, Item, Dict } = require('./models');
const { writeLargeMapToFile } = require('./utils/fileUtils');

// Import PostgreSQL models
const db = require('./models/postgresql');

// Global variables for models (keeping existing approach)
global.dict = new Dict('');
global.dicts = new Map();
global.user = new User('');
global.order = new Order('');
global.item = new Item('');
global.box = new Box('');
global.place = new Place('');
global.users = new Map();
global.orders = new Map();
global.uppers = new Map();
global.classes = new Map();
global.items = new Map();
global.boxes = new Map();
global.places = new Map();
global.runOnServer = Object.hasOwn(process.env, 'pm_id');
global.baseDirectory = __dirname + '/';

// Serial numbers for items/boxes/places
global.serialIi = 999999999999999;
global.serialI = 1;
global.serialB = 1;
global.serialP = 1;

// Create JWT secret
global.secretJwt = createSecretJwtKey(process.env.JWT_SECRET);




// Security middleware
app.use(helmet({
    contentSecurityPolicy: false, // Disable CSP to avoid issues with inline scripts
    crossOriginEmbedderPolicy: false
}));



// Initialize Passport
const configPassport = require('./config/passport');
const passportInstance = configPassport(global.secretJwt);
app.use(passport.initialize());

// Middleware
app.use(express.json());
app.use(express.urlencoded({ extended: true }));




app.use(bodyParser.text({ type: 'text/html' }));



// Serve static files
//app.use(express.static(path.join(__dirname, 'html')));

// Затем ваш логирующий middleware


//


// Logging middleware
app.use(morgan('[:date[clf]] :method :url :status :response-time ms - :res[content-length]'));
app.use(requestLogger);

/// Initialize i18n first
app.use(initI18n());

// Create HTML translator with access to i18next
const htmlTranslator = htmlInterceptor(i18next);
app.use(htmlTranslator);

// Request logging middleware
// app.use((req, res, next) => {
//     console.log('========= REQUEST HEADERS =========');
//     console.log('URL:', req.url);
//     console.log('Original URL:', req.originalUrl);
//     console.log('Cookies:', req.cookies);
//     console.log('Accept-Language:', req.headers['accept-language']);
//     console.log('Detected Language:', req.language);
//     console.log('Query:', req.query);
//     console.log('==================================');
//     next();
// });

// Serve static files last
app.use('/locales', (req, res, next) => {
    // Set cache control headers for translation files
    res.setHeader('Cache-Control', 'no-cache, no-store, must-revalidate');
    res.setHeader('Pragma', 'no-cache');
    res.setHeader('Expires', '0');
    next();
}, express.static(path.join(__dirname, 'html', 'locales')));
app.use(express.static(path.join(__dirname, 'html')));

// Routes
app.use('/api', apiRoutes);
app.use('/rma', rmaRoutes);
app.use('/status', statusRoutes);
app.use('/admin', adminRoutes);
app.use('/auth', authRoutes);
app.use('/api', translationApiRoutes);
app.use('/translation-admin', translationAdminRoutes);
// Main route for the application
app.get('/', (req, res) => {
    res.sendFile(path.join(__dirname, 'html', 'index.html'));
});

// Handle JWT verification route
app.get('/jwt/:token', (req, res) => {
    const { token } = req.params;
    res.status(200);
    res.setHeader('Content-Type', 'text/html');
    try {
        const { verifyJWT } = require('./utils/encryption');
        const fieldsToMask = ["comp", "pers", "str", "cem", "iem"];
        const payload = verifyJWT(token, global.secretJwt);
        const { prettyPrintObject, maskObjectFields } = require('./utils/formatUtils');
        const maskedObj = payload.a !== 'p' ? maskObjectFields(global.orders.get('o000' + payload.r), fieldsToMask) : global.orders.get('o000' + payload.r);
        res.send('<div style="width: min-content;">' + prettyPrintObject(maskedObj) + '</div><br><br><a href="https://m3.repair/" style="color:#1e2071;">M3 Mobile GmbH Homepage</a>');
    } catch (error) {
        res.send(' nice try');
    }
});

app.get('/admin/translations', requireAdmin, (req, res) => {
    res.sendFile(path.join(__dirname, 'html/admin/translations.html'));
});

// Main POST handler - handles various requests
app.post('/', async (req, res) => {
    try {
        const parsedData = req.body;

        if (parsedData.dest === 'csv') {
            const csvData = await generateCsvData();
            res.writeHead(200, {
                'Content-Type': 'text/csv',
                'Content-Disposition': 'attachment; filename="export.csv"',
                'Content-Length': csvData.length
            });
            res.end(csvData);
        } else {
            const { parseHtml } = require('./utils/htmlParser');
            const htmlContent = await parseHtml(parsedData);
            res.status(200).send(htmlContent);
        }
    } catch (error) {
        res.status(500).send('Server error: ' + error.message);
    }
});



// Logging function
async function writeLog(str) {
    const dateTemp = new Date(Date.now());
    const logDir = resolve('./logs');

    // Create logs directory if it doesn't exist
    if (!fs.existsSync(logDir)) {
        fs.mkdirSync(logDir, { recursive: true });
    }

    const filename = `${dateTemp.getUTCFullYear().toString().slice(-2)}${('00' + (dateTemp.getUTCMonth() + 1)).slice(-2)}.txt`;
    return appendFile(resolve(`./logs/${filename}`), `${str}\t\t\t\t\t${dateTemp.getUTCDate()}_${dateTemp.getUTCHours()}:${dateTemp.getUTCMinutes()}:${dateTemp.getUTCSeconds()}\n`);
}

// Helper function to generate CSV data
async function generateCsvData() {
    let csv = 'SN /PN;Model;IN DATE;Out Date;Customer;SKU;email;Address;Zip Code;City;Complaint;Verification;Cause;Result;Shipping;Invoice number;Special note; warranty;condition;Used New Parts;Used Refurbished Parts\n';

    // Generate CSV data logic from the original application
    // This is a simplified version - implement full logic based on original code

    boxes.forEach((element) => {
        let packIn = false;
        let packOut = false;
        element.loc?.forEach((locElement) => {
            if (locElement[0] == 'p000000000000000030') packIn = locElement[1];
            if (locElement[0] == 'p000000000000000060') packOut = locElement[1];
        });

        if (packIn || packOut) {
            // Process box data to generate CSV rows
            // Implement full logic from the original code
        }
    });

    return csv;
}

// Initialize data and start server
async function initialize() {
    const { initialisation, classesUpdate, upperUpdate } = require('./utils/dataInit');

    try {
        // Initialize PostgreSQL database connection
        await db.sequelize.authenticate();
        console.log('PostgreSQL connection established successfully.');

        // Sync models with database (in development only)
        if (process.env.NODE_ENV !== 'production') {
            await db.sequelize.sync({ alter: process.env.DB_ALTER === 'true' });
            console.log('PostgreSQL models synchronized');
        }

        // Initialize legacy data
        await initialisation(baseDirectory);
        console.log('Legacy data initialized');

        await writeLog('login ' + Object.values(process.memoryUsage()));

        classesUpdate();
        upperUpdate();

        // Setup graceful shutdown
        process.on('SIGINT', async function () {
            console.log('Shutting down...');
            await logOut(baseDirectory);
            process.exit(0);
        });

        // Start the server
        const PORT = process.env.PORT || 3000;
        app.listen(PORT, () => {
            console.log(`Server running on port ${PORT}`);
        });
    } catch (err) {
        console.error('Failed to initialize application:', err);
        process.exit(1);
    }
}

// Helper function for logging out (saving data)
async function logOut(mainDirectory) {
    try {
        await writeLargeMapToFile(users, resolve(`${mainDirectory}base/users.json`));
        await writeLargeMapToFile(orders, resolve(`${mainDirectory}base/orders.json`));
        await writeLargeMapToFile(items, resolve(`${mainDirectory}base/items.json`));
        await writeLargeMapToFile(boxes, resolve(`${mainDirectory}base/boxes.json`));
        await writeLargeMapToFile(places, resolve(`${mainDirectory}base/places.json`));
        await writeLargeMapToFile(classes, resolve(`${mainDirectory}base/classes.json`));
        await writeLargeMapToFile(uppers, resolve(`${mainDirectory}base/uppers.json`));
        await writeLargeMapToFile(dicts, resolve(`${mainDirectory}base/dicts.json`));
        await fs.promises.writeFile(resolve(`${mainDirectory}base/ini.json`), JSON.stringify({ serialIi, serialI, serialB, serialP }));
        await writeLog('logout ' + Object.values(process.memoryUsage()));
    } catch (err) {
        console.error(err);
    }
}

// Initialize and start the application
initialize();

// Add this to app.js or a maintenance service file
// Around initialization code

/**
 * Scheduled maintenance for translation system
 */
async function runTranslationMaintenance() {
    try {
        // Проверяем, что очередь существует перед использованием
        if (translationQueue) {
            const cleanedCount = translationQueue.cleanupStalled();
            if (cleanedCount > 0) {
                console.log(`Translation maintenance: Cleaned up ${cleanedCount} stalled jobs`);
            }
        } else {
            console.log('Translation queue not available for maintenance');
        }

        // Schedule next maintenance
        setTimeout(runTranslationMaintenance, 5 * 60 * 1000); // Every 5 minutes
    } catch (error) {
        console.error('Error in translation maintenance:', error);
        // Still schedule next run even if there was an error
        setTimeout(runTranslationMaintenance, 5 * 60 * 1000);
    }
}

// Start maintenance after app initialization
setTimeout(runTranslationMaintenance, 2 * 60 * 1000); // Start after 2 minutes


module.exports = app;