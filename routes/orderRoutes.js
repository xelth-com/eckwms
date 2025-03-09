// routes/orderRoutes.js
const express = require('express');
const router = express.Router();
const { authenticateJWT, requireElevated, requireRmaAccess } = require('../middleware/auth');
const { validateRmaNumber, validateRmaForm, checkValidationResult } = require('../utils/validators');
const orderController = require('../controllers/orderController');

/**
 * @route GET /api/orders
 * @desc Get all orders (with pagination and filtering)
 * @access Private (elevated permissions)
 */
router.get('/', 
  authenticateJWT,
  requireElevated,
  orderController.getAllOrders
);

/**
 * @route GET /api/orders/:rmaNumber
 * @desc Get order by RMA number
 * @access Private (owner or elevated permissions)
 */
router.get('/:rmaNumber', 
  authenticateJWT,
  validateRmaNumber(),
  checkValidationResult,
  orderController.getOrderByRmaNumber
);

/**
 * @route POST /api/orders
 * @desc Create a new RMA order
 * @access Public
 */
router.post('/', 
  validateRmaForm(),
  checkValidationResult,
  orderController.createOrder
);

/**
 * @route PUT /api/orders/:rmaNumber
 * @desc Update an RMA order
 * @access Private (elevated permissions)
 */
router.put('/:rmaNumber', 
  authenticateJWT,
  requireElevated,
  validateRmaNumber(),
  checkValidationResult,
  orderController.updateOrder
);

/**
 * @route POST /api/orders/:rmaNumber/items
 * @desc Add items to an RMA order
 * @access Private (elevated permissions)
 */
router.post('/:rmaNumber/items', 
  authenticateJWT,
  requireElevated,
  validateRmaNumber(),
  checkValidationResult,
  orderController.addItemsToOrder
);

/**
 * @route DELETE /api/orders/:rmaNumber/items/:itemSerialNumber
 * @desc Remove item from an RMA order
 * @access Private (elevated permissions)
 */
router.delete('/:rmaNumber/items/:itemSerialNumber', 
  authenticateJWT,
  requireElevated,
  validateRmaNumber(),
  checkValidationResult,
  orderController.removeItemFromOrder
);

/**
 * @route POST /api/orders/:rmaNumber/box
 * @desc Add box to an RMA order
 * @access Private
 */
router.post('/:rmaNumber/box', 
  authenticateJWT,
  validateRmaNumber(),
  checkValidationResult,
  orderController.addBoxToOrder
);

/**
 * @route GET /api/orders/:rmaNumber/export
 * @desc Export order data as PDF
 * @access Private (owner or elevated permissions)
 */
router.get('/:rmaNumber/export', 
  authenticateJWT,
  validateRmaNumber(),
  checkValidationResult,
  orderController.exportOrderPdf
);

/**
 * @route GET /api/orders/:rmaNumber/status
 * @desc Get order status
 * @access Public (with token)
 */
router.get('/:rmaNumber/status', 
  requireRmaAccess,
  validateRmaNumber(),
  checkValidationResult,
  orderController.getOrderStatus
);

module.exports = router;