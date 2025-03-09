// routes/itemRoutes.js
const express = require('express');
const router = express.Router();
const { authenticateJWT, requireElevated } = require('../middleware/auth');
const { 
  validateSerialNumber, 
  validateBarcode, 
  checkValidationResult 
} = require('../utils/validators');
const itemController = require('../controllers/itemController');

/**
 * @route GET /api/items
 * @desc Get all items (with pagination and filtering)
 * @access Private
 */
router.get('/', 
  authenticateJWT,
  itemController.getAllItems
);

/**
 * @route GET /api/items/:serialNumber
 * @desc Get item by serial number
 * @access Private
 */
router.get('/:serialNumber', 
  authenticateJWT,
  validateSerialNumber(),
  checkValidationResult,
  itemController.getItemBySerialNumber
);

/**
 * @route POST /api/items
 * @desc Create a new item
 * @access Private (elevated permissions)
 */
router.post('/', 
  authenticateJWT,
  requireElevated,
  itemController.createItem
);

/**
 * @route PUT /api/items/:serialNumber
 * @desc Update an item
 * @access Private (elevated permissions)
 */
router.put('/:serialNumber', 
  authenticateJWT,
  requireElevated,
  validateSerialNumber(),
  checkValidationResult,
  itemController.updateItem
);

/**
 * @route POST /api/items/:serialNumber/location
 * @desc Update item location
 * @access Private
 */
router.post('/:serialNumber/location', 
  authenticateJWT,
  validateSerialNumber(),
  checkValidationResult,
  itemController.updateItemLocation
);

/**
 * @route POST /api/items/:serialNumber/barcode
 * @desc Add barcode to item
 * @access Private
 */
router.post('/:serialNumber/barcode', 
  authenticateJWT,
  validateSerialNumber(),
  validateBarcode(),
  checkValidationResult,
  itemController.addBarcode
);

/**
 * @route POST /api/items/:serialNumber/action
 * @desc Add action to item (check, cause, result, note)
 * @access Private
 */
router.post('/:serialNumber/action', 
  authenticateJWT,
  validateSerialNumber(),
  checkValidationResult,
  itemController.addAction
);

/**
 * @route GET /api/items/:serialNumber/history
 * @desc Get item history
 * @access Private
 */
router.get('/:serialNumber/history', 
  authenticateJWT,
  validateSerialNumber(),
  checkValidationResult,
  itemController.getItemHistory
);

/**
 * @route GET /api/items/search/barcode/:barcode
 * @desc Find item by barcode
 * @access Private
 */
router.get('/search/barcode/:barcode', 
  authenticateJWT,
  validateBarcode(),
  checkValidationResult,
  itemController.findItemByBarcode
);

module.exports = router;