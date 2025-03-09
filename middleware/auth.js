// middleware/auth.js
const authService = require('../services/authService');
const logger = require('../utils/logging');

/**
 * Middleware to authenticate JWT token in requests
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function authenticateJWT(req, res, next) {
  try {
    // Get token from various sources
    const token = req.headers.authorization?.split(' ')[1] || 
                 req.cookies?.jwt ||
                 req.body?.jwt;
    
    if (!token) {
      return res.status(401).json({ message: 'Authentication required' });
    }
    
    try {
      const decoded = await authService.verifyToken(token);
      
      // Normalize any case-sensitive fields in the token payload
      if (decoded.r) {
        // For RMA tokens, ensure RMA number is case-insensitive
        decoded.r = decoded.r.toUpperCase();
      }
      
      req.user = decoded;
      next();
    } catch (error) {
      logger.warn(`JWT Authentication failed: ${error.message}`);
      return res.status(401).json({ message: 'Invalid or expired token' });
    }
  } catch (error) {
    logger.error(`Auth middleware error: ${error.message}`);
    return res.status(500).json({ message: 'Internal server error' });
  }
}

/**
 * Middleware to check if user has admin role
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
function requireAdmin(req, res, next) {
  if (req.user && req.user.a === 'a') {
    next();
  } else {
    logger.warn(`Access denied for user: ${req.user?.u || 'unknown'}`);
    return res.status(403).json({ message: 'Access denied. Admin privileges required.' });
  }
}

/**
 * Middleware to check if user has elevated permissions
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
function requireElevated(req, res, next) {
  if (req.user && (req.user.a === 'a' || req.user.a === 'p')) {
    next();
  } else {
    logger.warn(`Elevated access denied for user: ${req.user?.u || 'unknown'}`);
    return res.status(403).json({ message: 'Access denied. Elevated privileges required.' });
  }
}

/**
 * Middleware to check if user has RMA access
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
function requireRmaAccess(req, res, next) {
  if (req.user && req.user.r) {
    // Allow access only to this specific RMA, case-insensitive
    const requestedRma = req.params.rmaNumber?.replace(/^RMA/i, '').toUpperCase();
    const tokenRma = req.user.r.toUpperCase();
    
    if (requestedRma === tokenRma) {
      req.rmaNumber = requestedRma;
      next();
    } else {
      logger.warn(`RMA access denied for user: ${req.user?.u || 'unknown'}, requested: ${requestedRma}, allowed: ${tokenRma}`);
      return res.status(403).json({ message: 'Access denied. You do not have access to this RMA.' });
    }
  } else {
    logger.warn(`RMA access denied for user: ${req.user?.u || 'unknown'}`);
    return res.status(403).json({ message: 'Access denied. You do not have access to this RMA.' });
  }
}

module.exports = {
  authenticateJWT,
  requireAdmin,
  requireElevated,
  requireRmaAccess
};