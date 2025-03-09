// services/authService.js
const jwt = require('jsonwebtoken');
const { promisify } = require('util');
const logger = require('../utils/logging');

// Load JWT secret from environment variables
const JWT_SECRET = process.env.JWT_SECRET || '71e990ae4c9ef235687634fd4d9568eff4b0ca10af15c910c459e12347240855';
const JWT_EXPIRES_IN = '90d'; // 90 days

/**
 * Creates a JWT token for the specified user
 * @param {Object} payload - The data to include in the JWT
 * @returns {string} The JWT token
 */
function generateToken(payload) {
  return jwt.sign(payload, JWT_SECRET, {
    expiresIn: JWT_EXPIRES_IN
  });
}

/**
 * Verifies a JWT token and returns the decoded payload
 * @param {string} token - The JWT token to verify
 * @returns {Promise<Object>} The decoded token payload
 */
async function verifyToken(token) {
  try {
    const decoded = await promisify(jwt.verify)(token, JWT_SECRET);
    return decoded;
  } catch (error) {
    logger.error(`JWT Verification failed: ${error.message}`);
    throw new Error(`Invalid token: ${error.message}`);
  }
}

/**
 * Creates a token for RMA access with limited permissions
 * @param {string} rmaNumber - The RMA number
 * @param {string} accessType - The type of access ('l' for limited, 'p' for full)
 * @returns {string} The JWT token
 */
function createRmaToken(rmaNumber, accessType = 'l') {
  const expiryTime = accessType === 'l' 
    ? Math.floor(Date.now() / 1000) + (60 * 60 * 24 * 30) // 30 days
    : Math.floor(Date.now() / 1000) + (60 * 60 * 24 * 90); // 90 days
  
  return generateToken({
    r: rmaNumber.trim(),
    a: accessType,
    e: expiryTime
  });
}

/**
 * Creates a user authentication token
 * @param {string} username - The username
 * @param {string} accessLevel - The access level permissions
 * @returns {string} The JWT token
 */
function createUserToken(username, accessLevel = 'p') {
  return generateToken({
    u: username,
    a: accessLevel,
    e: Math.floor(Date.now() / 1000) + (60 * 60 * 24 * 90) // 90 days
  });
}

module.exports = {
  generateToken,
  verifyToken,
  createRmaToken,
  createUserToken
};