// utils/validators.js
const { body, param, query, validationResult } = require('express-validator');
const logger = require('./logging');

/**
 * Create validation rules for RMA form submission
 * @returns {Array} Array of validation chains
 */
function validateRmaForm() {
  return [
    body('rma')
      .trim()
      .matches(/^RMA\d{10}[A-Za-z0-9]{2}$/)
      .withMessage('Invalid RMA number format'),
    
    body('company')
      .trim()
      .notEmpty()
      .withMessage('Company name is required')
      .isLength({ max: 100 })
      .withMessage('Company name must be at most 100 characters'),
    
    body('person')
      .trim()
      .optional()
      .isLength({ max: 100 })
      .withMessage('Person name must be at most 100 characters'),
    
    body('street')
      .trim()
      .notEmpty()
      .withMessage('Street address is required')
      .isLength({ max: 100 })
      .withMessage('Street must be at most 100 characters'),
    
    body('postal')
      .trim()
      .notEmpty()
      .withMessage('Postal code and city are required')
      .isLength({ max: 100 })
      .withMessage('Postal code and city must be at most 100 characters'),
    
    body('country')
      .trim()
      .notEmpty()
      .withMessage('Country is required')
      .isLength({ max: 100 })
      .withMessage('Country must be at most 100 characters'),
    
    body('email')
      .trim()
      .notEmpty()
      .withMessage('Email is required')
      .isEmail()
      .withMessage('Invalid email format')
      .normalizeEmail(),
    
    body('invoice_email')
      .trim()
      .optional()
      .isEmail()
      .withMessage('Invalid email format')
      .normalizeEmail(),
    
    body('phone')
      .trim()
      .optional()
      .isLength({ max: 20 })
      .withMessage('Phone number must be at most 20 characters'),
    
    body('serial*')
      .trim()
      .optional()
      .isLength({ max: 20 })
      .withMessage('Serial number must be at most 20 characters'),
    
    body('description*')
      .trim()
      .optional()
      .isLength({ max: 500 })
      .withMessage('Description must be at most 500 characters')
  ];
}

/**
 * Validate search query
 * @returns {Array} Array of validation chains
 */
function validateSearch() {
  return [
    query('q')
      .trim()
      .notEmpty()
      .withMessage('Search query is required')
      .isLength({ max: 100 })
      .withMessage('Search query must be at most 100 characters')
  ];
}

/**
 * Validate serial number format
 * @returns {Array} Array of validation chains
 */
function validateSerialNumber() {
  return [
    param('serialNumber')
      .trim()
      .matches(/^\d{7}$/)
      .withMessage('Invalid serial number format')
  ];
}

/**
 * Validate RMA number format
 * @returns {Array} Array of validation chains
 */
function validateRmaNumber() {
  return [
    param('rmaNumber')
      .trim()
      .matches(/^RMA\d{10}[A-Za-z0-9]{2}$/)
      .withMessage('Invalid RMA number format')
  ];
}

/**
 * Validate barcodes
 * @returns {Array} Array of validation chains
 */
function validateBarcode() {
  return [
    body('barcode')
      .trim()
      .isLength({ min: 1, max: 100 })
      .withMessage('Barcode must be between 1 and 100 characters')
  ];
}

/**
 * Validate box item operations
 * @returns {Array} Array of validation chains
 */
function validateBoxItemOperation() {
  return [
    body('boxId')
      .trim()
      .matches(/^b\d{18}$/)
      .withMessage('Invalid box ID format'),
    
    body('itemId')
      .trim()
      .matches(/^i\d{18}$/)
      .withMessage('Invalid item ID format')
  ];
}

/**
 * Middleware to check validation results
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
function checkValidationResult(req, res, next) {
  const errors = validationResult(req);
  
  if (!errors.isEmpty()) {
    logger.warn(`Validation errors: ${JSON.stringify(errors.array())}`);
    return res.status(400).json({ 
      message: 'Validation failed',
      errors: errors.array() 
    });
  }
  
  next();
}

/**
 * Sanitize user input to prevent XSS
 * @param {string} input - User input
 * @returns {string} Sanitized input
 */
function sanitizeInput(input) {
  if (typeof input !== 'string') {
    return input;
  }
  
  return input
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#039;');
}

module.exports = {
  validateRmaForm,
  validateSearch,
  validateSerialNumber,
  validateRmaNumber,
  validateBarcode,
  validateBoxItemOperation,
  checkValidationResult,
  sanitizeInput
};