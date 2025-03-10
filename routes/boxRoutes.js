// routes/boxRoutes.js
const express = require('express');
const router = express.Router();
const { authenticateJWT, requireElevated } = require('../middleware/auth');
const { validateSerialNumber, validateBarcode, validateBoxItemOperation, checkValidationResult } = require('../utils/validators');
const boxController = require('../controllers/boxController');
const RequestHandler = require('../middleware/requestHandler');

/**
 * @route GET /api/boxes
 * @desc Get all boxes (with pagination and filtering)
 * @access Private
 */
router.get('/', 
  authenticateJWT,
  boxController.getAllBoxes
);

/**
 * @route GET /api/boxes/:serialNumber
 * @desc Get box by serial number
 * @access Private
 */
router.get('/:serialNumber', 
  authenticateJWT,
  validateSerialNumber(),
  checkValidationResult,
  RequestHandler.normalizeBoxIds,
  boxController.getBoxBySerialNumber
);

/**
 * @route POST /api/boxes
 * @desc Create a new box
 * @access Private (elevated permissions)
 */
router.post('/', 
  authenticateJWT,
  requireElevated,
  boxController.createBox
);

/**
 * @route PUT /api/boxes/:serialNumber
 * @desc Update a box
 * @access Private (elevated permissions)
 */
router.put('/:serialNumber', 
  authenticateJWT,
  requireElevated,
  validateSerialNumber(),
  checkValidationResult,
  RequestHandler.normalizeBoxIds,
  boxController.updateBox
);

/**
 * @route PUT /api/boxes/:serialNumber/location
 * @desc Update box location
 * @access Private (elevated permissions)
 */
router.put('/:serialNumber/location', 
  authenticateJWT,
  requireElevated,
  validateSerialNumber(),
  checkValidationResult,
  RequestHandler.normalizeBoxIds,
  boxController.updateBoxLocation
);

/**
 * @route POST /api/boxes/:serialNumber/items
 * @desc Add item to box
 * @access Private (elevated permissions)
 */
router.post('/:serialNumber/items', 
  authenticateJWT,
  requireElevated,
  validateSerialNumber(),
  checkValidationResult,
  RequestHandler.normalizeBoxIds,
  boxController.addItemToBox
);

/**
 * @route DELETE /api/boxes/:serialNumber/items/:itemId
 * @desc Remove item from box
 * @access Private (elevated permissions)
 */
router.delete('/:serialNumber/items/:itemId', 
  authenticateJWT,
  requireElevated,
  validateSerialNumber(),
  checkValidationResult,
  RequestHandler.normalizeBoxIds,
  RequestHandler.normalizeItemIds,
  boxController.removeItemFromBox
);

/**
 * @route POST /api/boxes/:serialNumber/barcode
 * @desc Add barcode to box
 * @access Private (elevated permissions)
 */
router.post('/:serialNumber/barcode', 
  authenticateJWT,
  requireElevated,
  validateSerialNumber(),
  validateBarcode(),
  checkValidationResult,
  RequestHandler.normalizeBoxIds,
  boxController.addBarcode
);

/**
 * @route GET /api/boxes/:serialNumber/contents
 * @desc Get box contents
 * @access Private
 */
router.get('/:serialNumber/contents', 
  authenticateJWT,
  validateSerialNumber(),
  checkValidationResult,
  RequestHandler.normalizeBoxIds,
  boxController.getBoxContents
);

/**
 * @route GET /api/boxes/:serialNumber/history
 * @desc Get box history
 * @access Private
 */
router.get('/:serialNumber/history', 
  authenticateJWT,
  validateSerialNumber(),
  checkValidationResult,
  RequestHandler.normalizeBoxIds,
  boxController.getBoxHistory
);

/**
 * @route GET /api/boxes/barcode/:barcode
 * @desc Find box by barcode
 * @access Private
 */
router.get('/barcode/:barcode', 
  authenticateJWT,
  validateBarcode(),
  checkValidationResult,
  boxController.findBoxByBarcode
);

module.exports = router;