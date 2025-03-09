// middleware/errorHandler.js
const logger = require('../utils/logging');

/**
 * Custom error class for API errors
 * @class ApiError
 * @extends Error
 */
class ApiError extends Error {
  /**
   * Create an API error
   * @param {string} message - Error message
   * @param {number} statusCode - HTTP status code
   * @param {Object} details - Additional error details
   */
  constructor(message, statusCode = 500, details = {}) {
    super(message);
    this.name = this.constructor.name;
    this.statusCode = statusCode;
    this.details = details;
    Error.captureStackTrace(this, this.constructor);
  }
}

/**
 * Not found error handler - Should be placed after all routes
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
const notFoundHandler = (req, res, next) => {
  const error = new ApiError(`Not found - ${req.originalUrl}`, 404);
  next(error);
};

/**
 * Global error handler
 * @param {Error} err - Error object
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
const errorHandler = (err, req, res, next) => {
  // Set default status code and error details
  const statusCode = err.statusCode || 500;
  const isDevelopment = process.env.NODE_ENV === 'development';
  
  // Log the error
  if (statusCode >= 500) {
    logger.error(`Server error: ${err.message}`, { 
      error: err,
      stack: err.stack
    });
  } else {
    logger.warn(`Client error: ${err.message}`, { 
      statusCode,
      error: err.name,
      details: err.details
    });
  }
  
  // Structure the error response
  const errorResponse = {
    error: {
      message: err.message,
      code: err.name,
      ...(isDevelopment && { stack: err.stack }),
      ...(err.details && { details: err.details })
    }
  };
  
  // Add request ID if available
  if (req.logger?.defaultMeta?.requestId) {
    errorResponse.requestId = req.logger.defaultMeta.requestId;
  }
  
  // Send error response
  res.status(statusCode).json(errorResponse);
};

/**
 * Create a not found error
 * @param {string} message - Error message
 * @returns {ApiError} Not found error
 */
const createNotFoundError = (message = 'Resource not found') => {
  return new ApiError(message, 404);
};

/**
 * Create a bad request error
 * @param {string} message - Error message
 * @param {Object} details - Additional error details
 * @returns {ApiError} Bad request error
 */
const createBadRequestError = (message = 'Bad request', details = {}) => {
  return new ApiError(message, 400, details);
};

/**
 * Create an unauthorized error
 * @param {string} message - Error message
 * @returns {ApiError} Unauthorized error
 */
const createUnauthorizedError = (message = 'Unauthorized') => {
  return new ApiError(message, 401);
};

/**
 * Create a forbidden error
 * @param {string} message - Error message
 * @returns {ApiError} Forbidden error
 */
const createForbiddenError = (message = 'Forbidden') => {
  return new ApiError(message, 403);
};

/**
 * Create a conflict error
 * @param {string} message - Error message
 * @param {Object} details - Additional error details
 * @returns {ApiError} Conflict error
 */
const createConflictError = (message = 'Conflict', details = {}) => {
  return new ApiError(message, 409, details);
};

/**
 * Create a server error
 * @param {string} message - Error message
 * @param {Object} details - Additional error details
 * @returns {ApiError} Server error
 */
const createServerError = (message = 'Internal server error', details = {}) => {
  return new ApiError(message, 500, details);
};

module.exports = {
  ApiError,
  notFoundHandler,
  errorHandler,
  createNotFoundError,
  createBadRequestError,
  createUnauthorizedError,
  createForbiddenError,
  createConflictError,
  createServerError
};