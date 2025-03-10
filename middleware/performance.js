// middleware/performance.js
const compression = require('compression');
const helmet = require('helmet');
const cors = require('cors');
const rateLimit = require('express-rate-limit');
const slowDown = require('express-slow-down');
const logger = require('../utils/logging');

/**
 * Configure and return compression middleware
 * @returns {Function} Compression middleware
 */
const setupCompression = () => {
  return compression({
    level: 6, // Compression level (0-9)
    threshold: 1024, // Minimum size in bytes to compress response
    filter: (req, res) => {
      // Don't compress responses for requests that have a 'Cache-Control: no-transform' header
      if (req.headers['cache-control']?.includes('no-transform')) {
        return false;
      }
      // Use compression by default
      return compression.filter(req, res);
    }
  });
};

/**
 * Configure and return security headers middleware (Helmet)
 * @returns {Function} Helmet middleware
 */
const setupSecurity = () => {
  return helmet({
    contentSecurityPolicy: {
      directives: {
        defaultSrc: ["'self'"],
        scriptSrc: ["'self'", "'unsafe-inline'", "https://cdnjs.cloudflare.com"],
        styleSrc: ["'self'", "'unsafe-inline'", "https://fonts.googleapis.com"],
        fontSrc: ["'self'", "https://fonts.gstatic.com"],
        imgSrc: ["'self'", "data:", "https://*.m3mobile.com"],
        connectSrc: ["'self'"],
        frameSrc: ["'none'"],
        objectSrc: ["'none'"],
        upgradeInsecureRequests: []
      }
    },
    hsts: {
      maxAge: 31536000, // 1 year in seconds
      includeSubDomains: true,
      preload: true
    }
  });
};

/**
 * Configure and return CORS middleware
 * @returns {Function} CORS middleware
 */
const setupCors = () => {
  return cors({
    origin: process.env.ALLOWED_ORIGINS?.split(',') || '*',
    methods: ['GET', 'POST', 'PUT', 'DELETE', 'OPTIONS'],
    allowedHeaders: ['Content-Type', 'Authorization', 'X-Requested-With'],
    exposedHeaders: ['Content-Disposition'],
    credentials: true,
    maxAge: 86400 // 1 day in seconds
  });
};

/**
 * Configure and return rate limiting middleware
 * @returns {Function} Rate limiting middleware
 */
const setupRateLimit = () => {
  const limiter = rateLimit({
    windowMs: 15 * 60 * 1000, // 15 minutes
    max: 100, // Limit each IP to 100 requests per windowMs
    standardHeaders: true, // Return rate limit info in the `RateLimit-*` headers
    legacyHeaders: false, // Disable the `X-RateLimit-*` headers
    message: {
      error: {
        message: 'Too many requests, please try again later.',
        code: 'RATE_LIMIT_EXCEEDED'
      }
    },
    handler: (req, res, next, options) => {
      logger.warn(`Rate limit exceeded for IP: ${req.ip}`);
      res.status(options.statusCode).json(options.message);
    },
    keyGenerator: (req) => {
      // Use authenticated user ID if available, otherwise use IP
      return req.user?.u || req.ip;
    },
    skip: (req) => {
      // Skip rate limiting for health check endpoint
      return req.path === '/api/health';
    }
  });
  
  return limiter;
};

/**
 * Configure and return speed limiting middleware
 * @returns {Function} Speed limiting middleware
 */
const setupSpeedLimit = () => {
  const speedLimiter = slowDown({
    windowMs: 15 * 60 * 1000, // 15 minutes
    delayAfter: 50, // Allow 50 requests per 15 minutes window
    delayMs: (hits) => hits * 100, // Add 100ms of delay per request above delayAfter
    maxDelayMs: 2000, // Maximum delay per request is 2 seconds
    keyGenerator: (req) => {
      // Use authenticated user ID if available, otherwise use IP
      return req.user?.u || req.ip;
    },
    skip: (req) => {
      // Skip speed limiting for health check endpoint
      return req.path === '/api/health';
    },
    onLimitReached: (req, res, options) => {
      logger.info(`Speed limit active for IP: ${req.ip}, delay: ${options.delayMs}ms`);
    }
  });
  
  return speedLimiter;
};

/**
 * Set up all performance middleware
 * @param {Object} app - Express app
 */
const setupPerformanceMiddleware = (app) => {
  // Apply middleware
  app.use(setupCompression());
  app.use(setupSecurity());
  app.use(setupCors());
  
  // Apply rate limiting and speed limiting
  if (process.env.NODE_ENV === 'production') {
    app.use(setupRateLimit());
    app.use(setupSpeedLimit());
  }
  
// Set up request ID and response time tracking
app.use((req, res, next) => {
  req.requestId = req.headers['x-request-id'] || 
                 `req-${Date.now()}-${Math.random().toString(36).substr(2, 9)}`;
  res.setHeader('X-Request-Id', req.requestId);
  
  req.startTime = Date.now();
  
  // Store original end method
  const originalEnd = res.end;
  
  // Override end method
  res.end = function(chunk, encoding) {
    try {
      // Add response time
      const responseTime = Date.now() - req.startTime;
      
      // Only set header if headers haven't been sent yet
      if (!res.headersSent) {
        res.setHeader('X-Response-Time', `${responseTime}ms`);
      }
    } catch (error) {
      // Ignore errors when setting headers
      console.error('Error setting response time header:', error.message);
    }
    
    // Call original end method
    return originalEnd.apply(this, arguments);
  };
  
  next();
});
};

module.exports = {
  setupPerformanceMiddleware,
  setupCompression,
  setupSecurity,
  setupCors,
  setupRateLimit,
  setupSpeedLimit
};