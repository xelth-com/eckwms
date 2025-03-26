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

// Core middleware
app.use(express.json());
app.use(express.urlencoded({ extended: true }));
app.use(bodyParser.text({ type: 'text/html' }));

// Logging middleware - applied early but after compression
app.use(morgan('[:date[clf]] :method :url :status :response-time ms - :res[content-length]'));
app.use(requestLogger);

// Specific routes for heavily accessed static assets to apply strong caching
app.get('/favicon*.png', (req, res) => {
  res.set({
    'Cache-Control': `public, max-age=${60 * 60 * 24 * 30}`, // 30 days
    'Expires': new Date(Date.now() + 60 * 60 * 24 * 30 * 1000).toUTCString()
  });
  res.sendFile(path.join(__dirname, 'html', req.path));
});

app.get('/android-chrome-*.png', (req, res) => {
  res.set({
    'Cache-Control': `public, max-age=${60 * 60 * 24 * 30}`, // 30 days
    'Expires': new Date(Date.now() + 60 * 60 * 24 * 30 * 1000).toUTCString()
  });
  res.sendFile(path.join(__dirname, 'html', req.path));
});

app.get('/apple-touch-icon*.png', (req, res) => {
  res.set({
    'Cache-Control': `public, max-age=${60 * 60 * 24 * 30}`, // 30 days
    'Expires': new Date(Date.now() + 60 * 60 * 24 * 30 * 1000).toUTCString()
  });
  res.sendFile(path.join(__dirname, 'html', req.path));
});

app.get('/mstile-*.png', (req, res) => {
  res.set({
    'Cache-Control': `public, max-age=${60 * 60 * 24 * 30}`, // 30 days
    'Expires': new Date(Date.now() + 60 * 60 * 24 * 30 * 1000).toUTCString()
  });
  res.sendFile(path.join(__dirname, 'html', req.path));
});

// Static file middleware - BEFORE i18n and HTML processing
// Serve static files with different cache settings for different file types
app.use(express.static(path.join(__dirname, 'html'), {
  setHeaders: (res, filePath) => {
    const ext = path.extname(filePath).toLowerCase();
    
    // Apply appropriate caching based on file type
    if (['.png', '.jpg', '.jpeg', '.gif', '.webp', '.svg', '.ico'].includes(ext)) {
      // Images
      res.set('Cache-Control', `public, max-age=${60 * 60 * 24 * 7}`); // 7 days
    } else if (['.css', '.js'].includes(ext)) {
      // CSS/JS files with medium caching
      res.set('Cache-Control', `public, max-age=${60 * 60 * 24 * 3}`); // 3 days
    } else if (['.woff', '.woff2', '.ttf', '.eot', '.otf'].includes(ext)) {
      // Font files with long caching
      res.set('Cache-Control', `public, max-age=${60 * 60 * 24 * 30}`); // 30 days
    } else {
      // Other static files with shorter caching
      res.set('Cache-Control', `public, max-age=${60 * 60 * 24}`); // 1 day
    }
  }
}));

// Special handling for locale files
app.use('/locales', (req, res, next) => {
  // Different cache strategy for locale files
  res.setHeader('Cache-Control', 'no-cache, no-store, must-revalidate');
  res.setHeader('Pragma', 'no-cache');
  res.setHeader('Expires', '0');
  next();
}, express.static(path.join(__dirname, 'html', 'locales')));

// Initialize i18n AFTER static files
app.use(initI18n());

// Create HTML translator with access to i18next - AFTER static files
const htmlTranslator = htmlInterceptor(i18next);
app.use(htmlTranslator);

// Add global middleware to set language in response headers
app.use((req, res, next) => {
  // If language was determined by i18n middleware, add it to response headers
  if (req.language) {
    res.setHeader('Content-Language', req.language);
    
    // Also add a meta tag for client-side detection
    const originalSend = res.send;
    res.send = function(body) {
      if (typeof body === 'string' && 
          (res.get('Content-Type')?.includes('text/html') || 
           body.includes('<head>') || 
           body.includes('<!DOCTYPE html>'))) {
        
        // Only modify HTML responses
        const languageMeta = `<meta name="app-language" content="${req.language}">`;
        
        // Add the meta tag inside the head if possible
        if (body.includes('<head>')) {
          body = body.replace('<head>', `<head>\n    ${languageMeta}`);
        }
      }
      
      return originalSend.call(this, body);
    };
  }
  
  next();
});

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

// Add this to app.js or a maintenance service file
// Around initialization code

async function runTranslationMaintenance() {
    try {
      if (translationQueue) {
        const stats = translationQueue.getStats();
        console.log(`Translation maintenance: Queue contains ${stats.queuedItems} items, processing ${stats.processingItems} items`);
        
        const cleanedCount = translationQueue.cleanupStalled();
        if (cleanedCount > 0) {
          console.log(`Translation maintenance: Cleaned up ${cleanedCount} stalled jobs`);
        }
      }
      setTimeout(runTranslationMaintenance, 5 * 60 * 1000);
    } catch (error) {
      console.error('Error in translation maintenance:', error);
      setTimeout(runTranslationMaintenance, 5 * 60 * 1000);
    }
}

// Start maintenance after app initialization
setTimeout(runTranslationMaintenance, 2 * 60 * 1000); // Start after 2 minutes

// Start the app initialization
initialize();

module.exports = app;