// routes/userRoutes.js
const express = require('express');
const router = express.Router();
const { authenticateJWT, requireAdmin } = require('../middleware/auth');
const { validateUserId, validateUserCreate, validateUserUpdate, validatePasswordChange, validateLogin, checkValidationResult } = require('../utils/validators');
const userController = require('../controllers/userController');

/**
 * @route GET /api/users
 * @desc Get all users (admin only)
 * @access Private (admin)
 */
router.get('/', 
  authenticateJWT,
  requireAdmin,
  userController.getAllUsers
);

/**
 * @route GET /api/users/:userId
 * @desc Get user by ID
 * @access Private (admin or self)
 */
router.get('/:userId', 
  authenticateJWT,
  validateUserId(),
  checkValidationResult,
  userController.getUserById
);

/**
 * @route POST /api/users
 * @desc Create a new user (admin only)
 * @access Private (admin)
 */
router.post('/', 
  authenticateJWT,
  requireAdmin,
  validateUserCreate(),
  checkValidationResult,
  userController.createUser
);

/**
 * @route PUT /api/users/:userId
 * @desc Update a user
 * @access Private (admin or self)
 */
router.put('/:userId', 
  authenticateJWT,
  validateUserId(),
  validateUserUpdate(),
  checkValidationResult,
  userController.updateUser
);

/**
 * @route PUT /api/users/:userId/password
 * @desc Change user password
 * @access Private (admin or self)
 */
router.put('/:userId/password', 
  authenticateJWT,
  validateUserId(),
  validatePasswordChange(),
  checkValidationResult,
  userController.changePassword
);

/**
 * @route DELETE /api/users/:userId
 * @desc Delete a user (admin only)
 * @access Private (admin)
 */
router.delete('/:userId', 
  authenticateJWT,
  requireAdmin,
  validateUserId(),
  checkValidationResult,
  userController.deleteUser
);

/**
 * @route POST /api/users/login
 * @desc User login
 * @access Public
 */
router.post('/login', 
  validateLogin(),
  checkValidationResult,
  userController.login
);

/**
 * @route GET /api/users/me
 * @desc Get current user profile
 * @access Private
 */
router.get('/me', 
  authenticateJWT,
  userController.getCurrentUser
);

module.exports = router;