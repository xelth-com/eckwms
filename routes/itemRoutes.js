// routes/itemRoutes.js
const express = require('express');
const router = express.Router();
const { authenticateJWT, requireElevated } = require('../middleware/auth');
const { validateSerialNumber, validateBarcode, checkValidationResult } = require('../utils/validators');
const itemController = require('../controllers/itemController');
const RequestHandler = require('../middleware/requestHandler');

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
  RequestHandler.normalizeItemIds,
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
  RequestHandler.normalizeItemIds,
  itemController.updateItem
);

/**
 * @route PUT /api/items/:serialNumber/location
 * @desc Update item location
 * @access Private (elevated permissions)
 */
router.put('/:serialNumber/location', 
  authenticateJWT,
  requireElevated,
  validateSerialNumber(),
  checkValidationResult,
  RequestHandler.normalizeItemIds,
  itemController.updateItemLocation
);

/**
 * @route POST /api/items/:serialNumber/barcode
 * @desc Add barcode to item
 * @access Private (elevated permissions)
 */
router.post('/:serialNumber/barcode', 
  authenticateJWT,
  requireElevated,
  validateSerialNumber(),
  validateBarcode(),
  checkValidationResult,
  RequestHandler.normalizeItemIds,
  itemController.addBarcode
);

/**
 * @route POST /api/items/:serialNumber/action
 * @desc Add action to item
 * @access Private (elevated permissions)
 */
router.post('/:serialNumber/action', 
  authenticateJWT,
  requireElevated,
  validateSerialNumber(),
  checkValidationResult,
  RequestHandler.normalizeItemIds,
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
  RequestHandler.normalizeItemIds,
  itemController.getItemHistory
);

/**
 * @route GET /api/items/barcode/:barcode
 * @desc Find item by barcode
 * @access Private
 */
router.get('/barcode/:barcode', 
  authenticateJWT,
  validateBarcode(),
  checkValidationResult,
  itemController.findItemByBarcode
);

module.exports = router;