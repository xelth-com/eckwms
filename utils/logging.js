// utils/logging.js
const winston = require('winston');
const path = require('path');
const fs = require('fs');

// Ensure log directory exists
const logDir = 'logs';
if (!fs.existsSync(logDir)) {
  fs.mkdirSync(logDir);
}

// Define custom format for better error formatting
const betterErrorFormat = winston.format(info => {
  if (info.error && info.error instanceof Error) {
    // Extract full error details including stack trace
    info.errorDetails = {
      message: info.error.message,
      stack: info.error.stack,
      ...info.error
    };
  }
  return info;
});

// Define log format
const logFormat = winston.format.combine(
  betterErrorFormat(),
  winston.format.timestamp({ format: 'YYYY-MM-DD HH:mm:ss' }),
  winston.format.errors({ stack: true }),
  winston.format.splat(),
  winston.format.json()
);

// Get log level from environment or use default
const logLevel = process.env.LOG_LEVEL || 'info';

// Create the logger
const logger = winston.createLogger({
  level: logLevel,
  format: logFormat,
  defaultMeta: { service: 'wms-api' },
  transports: [
    // Write logs to console
    new winston.transports.Console({
      format: winston.format.combine(
        winston.format.colorize(),
        winston.format.printf(
          ({ level, message, timestamp, errorDetails, ...meta }) => {
            // Format error output nicely for console
            const stack = errorDetails?.stack ? `\n${errorDetails.stack}` : '';
            const metadata = Object.keys(meta).length > 0 && 
              !['stack', 'service'].includes(Object.keys(meta)[0]) ? 
              `\n${JSON.stringify(meta, null, 2)}` : '';
            
            return `${timestamp} ${level}: ${message}${stack}${metadata}`;
          }
        )
      )
    }),
    
    // Write all logs to file with rotation
    new winston.transports.File({ 
      filename: path.join(logDir, 'error.log'), 
      level: 'error',
      maxsize: 5242880, // 5MB
      maxFiles: 5,
    }),
    new winston.transports.File({ 
      filename: path.join(logDir, 'combined.log'),
      maxsize: 5242880, // 5MB
      maxFiles: 5,
    })
  ]
});

// Add request context middleware
logger.middleware = (req, res, next) => {
  // Add request ID to context
  const requestId = req.headers['x-request-id'] || 
                   `req-${Date.now()}-${Math.random().toString(36).substr(2, 9)}`;
  
  // Create child logger with request context
  req.logger = logger.child({
    requestId,
    method: req.method,
    url: req.originalUrl,
    ip: req.ip,
    userAgent: req.headers['user-agent'] || 'unknown',
    userId: req.user?.u || 'anonymous'
  });
  
  // Add response time tracking
  req.startTime = Date.now();
  
  // Log when response is finished
  res.on('finish', () => {
    const responseTime = Date.now() - req.startTime;
    const level = res.statusCode >= 500 ? 'error' : 
                 res.statusCode >= 400 ? 'warn' : 'info';
    
    req.logger[level]({
      message: `${req.method} ${req.originalUrl} ${res.statusCode}`,
      statusCode: res.statusCode,
      responseTime: `${responseTime}ms`
    });
  });
  
  next();
};

// Create audit log function
logger.audit = (action, userId, details = {}) => {
  logger.info(`AUDIT: ${action}`, {
    audit: true,
    userId,
    action,
    timestamp: new Date().toISOString(),
    details
  });
};

// Enhanced error handling for uncaught exceptions and unhandled rejections
process.on('uncaughtException', (error) => {
  logger.error(`Uncaught exception: ${error.message}`, { 
    error,
    stack: error.stack
  });
  
  // Give logger time to flush before exiting
  setTimeout(() => {
    process.exit(1);
  }, 1000);
});

process.on('unhandledRejection', (reason, promise) => {
  const error = reason instanceof Error ? reason : new Error(String(reason));
  logger.error(`Unhandled rejection: ${error.message}`, { 
    error,
    stack: error.stack,
    promise: String(promise)
  });
});

module.exports = logger;