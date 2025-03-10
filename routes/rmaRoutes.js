// routes/rmaRoutes.js
const express = require('express');
const router = express.Router();
const { authenticateJWT, requireRmaAccess } = require('../middleware/auth');
const { validateRmaForm, validateRmaNumber, checkValidationResult } = require('../utils/validators');
const rmaController = require('../controllers/rmaController');

/**
 * @route POST /api/rma
 * @desc Create a new RMA request
 * @access Public
 */
router.post('/', 
  validateRmaForm(),
  checkValidationResult,
  rmaController.createRmaRequest
);

/**
 * @route GET /api/rma/:rmaNumber
 * @desc Get RMA details by RMA number
 * @access Private (owner or admin)
 */
router.get('/:rmaNumber',
  authenticateJWT,
  requireRmaAccess,
  validateRmaNumber(),
  checkValidationResult,
  rmaController.getRmaDetails
);

/**
 * @route GET /api/rma/status
 * @desc Check RMA status
 * @access Public
 */
router.get('/status',
  rmaController.checkRmaStatus
);

module.exports = router;