// app.js
const express = require('express');
const path = require('path');
const cookieParser = require('cookie-parser');
const { setupPerformanceMiddleware } = require('./middleware/performance');
const logger = require('./utils/logging');
const routes = require('./routes');
const { notFoundHandler, errorHandler } = require('./middleware/errorHandler');
const StorageService = require('./services/storageService');
const HistoryService = require('./services/historyService');
const fs = require('fs').promises;

// Create Express app
const app = express();

// Initialize services
async function initializeServices() {
  try {
    // Configure base directory from environment variable or use default
    const baseDirectory = process.env.BASE_DIRECTORY || './';
    
    // Create necessary directories if they don't exist
    try {
      await fs.mkdir(path.join(baseDirectory, 'base'), { recursive: true });
      await fs.mkdir(path.join(baseDirectory, 'history'), { recursive: true });
      
      logger.info('Ensured required directories exist');
    } catch (error) {
      logger.error(`Failed to create necessary directories: ${error.message}`);
      // Continue with initialization despite directory creation errors
    }
    
    // Initialize storage service
    global.storageService = new StorageService(baseDirectory);
    const storageInitialized = await global.storageService.initialize().catch(error => {
      logger.error(`Storage service initialization error: ${error.message}`, error);
      return false;
    });
    
    if (!storageInitialized) {
      logger.error('Storage service failed to initialize properly');
      return false;
    }
    
    // Initialize history service
    global.historyService = new HistoryService(baseDirectory);
    const historyInitialized = await global.historyService.initialize().catch(error => {
      logger.error(`History service initialization error: ${error.message}`, error);
      return false;
    });
    
    if (!historyInitialized) {
      logger.error('History service failed to initialize properly');
      return false;
    }
    
    logger.info('All services initialized successfully');
    
    // Set up history cleanup interval (once a day)
    setInterval(async () => {
      try {
        await global.historyService.cleanupOldHistory();
        await global.storageService.cleanupHistory();
      } catch (error) {
        logger.error(`Error during history cleanup: ${error.message}`);
      }
    }, 24 * 60 * 60 * 1000);
    
    // Set up storage auto-save interval (every 5 minutes)
    setInterval(async () => {
      try {
        await global.storageService.saveAll();
      } catch (error) {
        logger.error(`Error during storage auto-save: ${error.message}`);
      }
    }, 5 * 60 * 1000);
    
    return true;
  } catch (error) {
    logger.error('Failed to initialize services:', error);
    return false;
  }
}

// Setup middleware
app.use(logger.middleware); // Request logging
setupPerformanceMiddleware(app); // Performance middleware (compression, security, CORS)
app.use(express.json()); // Parse JSON request bodies
app.use(express.urlencoded({ extended: false })); // Parse URL-encoded bodies
app.use(cookieParser()); // Parse cookies
app.use(express.static(path.join(__dirname, 'public'))); // Serve static files

// Ensure services are initialized before processing requests
app.use(async (req, res, next) => {
  if (!global.storageService || !global.storageService.initialized ||
      !global.historyService || !global.historyService.initialized) {
    
    // Try to initialize services
    const initialized = await initializeServices();
    
    if (!initialized) {
      return res.status(503).json({ 
        message: 'Service temporarily unavailable. Please try again later.'
      });
    }
  }
  
  next();
});

// Mount routes
app.use('/', routes);

// Error handling
app.use(notFoundHandler);
app.use(errorHandler);

// Initialize services on startup
initializeServices().then(initialized => {
  if (!initialized) {
    logger.error('Failed to initialize services. Application may not function correctly.');
  }
});

// Handle uncaught exceptions
process.on('unhandledRejection', (reason, promise) => {
  logger.error('Unhandled rejection:', { reason, promise });
});

module.exports = app;