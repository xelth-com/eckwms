// routes/userRoutes.js
const express = require('express');
const router = express.Router();
const { authenticateJWT, requireAdmin } = require('../middleware/auth');
const { body, param, checkValidationResult } = require('../utils/validators');
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
  param('userId').trim().notEmpty().withMessage('User ID is required'),
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
  [
    body('username').trim().notEmpty().withMessage('Username is required')
      .isLength({ min: 3, max: 50 }).withMessage('Username must be between 3 and 50 characters'),
    body('password').trim().notEmpty().withMessage('Password is required')
      .isLength({ min: 8 }).withMessage('Password must be at least 8 characters'),
    body('email').trim().notEmpty().withMessage('Email is required')
      .isEmail().withMessage('Invalid email format'),
    body('role').optional().isIn(['u', 'p', 'a']).withMessage('Invalid role')
  ],
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
  param('userId').trim().notEmpty().withMessage('User ID is required'),
  [
    body('email').optional().isEmail().withMessage('Invalid email format'),
    body('company').optional().trim(),
    body('phone').optional().trim(),
    body('street').optional().trim(),
    body('postalCode').optional().trim(),
    body('city').optional().trim(),
    body('country').optional().trim(),
    body('role').optional().isIn(['u', 'p', 'a']).withMessage('Invalid role')
  ],
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
  param('userId').trim().notEmpty().withMessage('User ID is required'),
  [
    body('currentPassword').optional(),
    body('newPassword').trim().notEmpty().withMessage('New password is required')
      .isLength({ min: 8 }).withMessage('New password must be at least 8 characters')
  ],
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
  param('userId').trim().notEmpty().withMessage('User ID is required'),
  checkValidationResult,
  userController.deleteUser
);

/**
 * @route POST /api/users/login
 * @desc User login
 * @access Public
 */
router.post('/login', 
  [
    body('username').trim().notEmpty().withMessage('Username is required'),
    body('password').trim().notEmpty().withMessage('Password is required')
  ],
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