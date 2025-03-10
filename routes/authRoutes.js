// routes/authRoutes.js
const express = require('express');
const router = express.Router();
const { validateLogin, checkValidationResult } = require('../utils/validators');
const userController = require('../controllers/userController');
const { authenticateJWT } = require('../middleware/auth');

/**
 * @route POST /api/auth/login
 * @desc User login
 * @access Public
 */
router.post('/login', 
  validateLogin(),
  checkValidationResult,
  userController.login
);

/**
 * @route POST /api/auth/logout
 * @desc User logout
 * @access Public
 */
router.post('/logout', (req, res) => {
  // No server-side action needed for logout, just inform client
  res.status(200).json({ message: 'Successfully logged out' });
});

/**
 * @route GET /api/auth/validate
 * @desc Validate JWT token
 * @access Private
 */
router.get('/validate', 
  authenticateJWT,
  (req, res) => {
    // If we got this far, the token is valid
    res.status(200).json({ 
      valid: true, 
      user: {
        id: req.user.u,
        role: req.user.a
      }
    });
  }
);

module.exports = router;