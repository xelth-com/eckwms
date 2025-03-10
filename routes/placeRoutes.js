// routes/placeRoutes.js
const express = require('express');
const router = express.Router();
const { authenticateJWT, requireElevated } = require('../middleware/auth');
const { validateSerialNumber, checkValidationResult } = require('../utils/validators');
const placeController = require('../controllers/placeController');

/**
 * @route GET /api/places
 * @desc Get all places (with pagination and filtering)
 * @access Private
 */
router.get('/', 
  authenticateJWT,
  placeController.getAllPlaces
);

/**
 * @route GET /api/places/:serialNumber
 * @desc Get place by serial number
 * @access Private
 */
router.get('/:serialNumber', 
  authenticateJWT,
  validateSerialNumber(),
  checkValidationResult,
  placeController.getPlaceBySerialNumber
);

/**
 * @route POST /api/places
 * @desc Create a new place
 * @access Private (elevated permissions)
 */
router.post('/', 
  authenticateJWT,
  requireElevated,
  placeController.createPlace
);

/**
 * @route PUT /api/places/:serialNumber
 * @desc Update a place
 * @access Private (elevated permissions)
 */
router.put('/:serialNumber', 
  authenticateJWT,
  requireElevated,
  validateSerialNumber(),
  checkValidationResult,
  placeController.updatePlace
);

/**
 * @route GET /api/places/:serialNumber/contents
 * @desc Get place contents
 * @access Private
 */
router.get('/:serialNumber/contents', 
  authenticateJWT,
  validateSerialNumber(),
  checkValidationResult,
  placeController.getPlaceContents
);

/**
 * @route GET /api/places/:serialNumber/hierarchy
 * @desc Get place hierarchy (parent chain)
 * @access Private
 */
router.get('/:serialNumber/hierarchy', 
  authenticateJWT,
  validateSerialNumber(),
  checkValidationResult,
  placeController.getPlaceHierarchy
);

/**
 * @route GET /api/places/:serialNumber/children
 * @desc Get children places
 * @access Private
 */
router.get('/:serialNumber/children', 
  authenticateJWT,
  validateSerialNumber(),
  checkValidationResult,
  placeController.getPlaceChildren
);

/**
 * @route POST /api/places/:serialNumber/items
 * @desc Add items to a place
 * @access Private (elevated permissions)
 */
router.post('/:serialNumber/items', 
  authenticateJWT,
  requireElevated,
  validateSerialNumber(),
  checkValidationResult,
  placeController.addItemsToPlace
);

/**
 * @route POST /api/places/:serialNumber/boxes
 * @desc Add boxes to a place
 * @access Private (elevated permissions)
 */
router.post('/:serialNumber/boxes', 
  authenticateJWT,
  requireElevated,
  validateSerialNumber(),
  checkValidationResult,
  placeController.addBoxesToPlace
);

/**
 * @route DELETE /api/places/:serialNumber/items/:itemId
 * @desc Remove item from place
 * @access Private (elevated permissions)
 */
router.delete('/:serialNumber/items/:itemId', 
  authenticateJWT,
  requireElevated,
  validateSerialNumber(),
  checkValidationResult,
  placeController.removeItemFromPlace
);

module.exports = router;