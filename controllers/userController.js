// controllers/userController.js
const crypto = require('crypto');
const jwt = require('jsonwebtoken');
const logger = require('../utils/logging');
const { ApiError, createNotFoundError, createBadRequestError, createUnauthorizedError, createForbiddenError } = require('../middleware/errorHandler');
const authService = require('../services/authService');

/**
 * Get all users with pagination and filtering
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function getAllUsers(req, res, next) {
  try {
    // Check if requester has admin privileges
    if (!req.user || req.user.a !== 'a') {
      return next(createForbiddenError('Admin privileges required'));
    }

    // Extract query parameters
    const page = parseInt(req.query.page) || 1;
    const limit = parseInt(req.query.limit) || 20;
    const search = req.query.search || '';
    const role = req.query.role || '';
    
    // Validate parameters
    if (page < 1 || limit < 1 || limit > 100) {
      return next(createBadRequestError('Invalid pagination parameters'));
    }
    
    // Get users from storage service
    const usersCollection = global.storageService.getCollection('users');
    if (!usersCollection) {
      return next(createNotFoundError('Users collection not found'));
    }
    
    // Filter users based on query parameters
    let filteredUsers = Array.from(usersCollection.values());
    
    // Apply search filter (case-insensitive)
    if (search) {
      const searchLower = search.toLowerCase();
      filteredUsers = filteredUsers.filter(user => {
        // Search in username
        if (user.nm && user.nm.toLowerCase().includes(searchLower)) {
          return true;
        }
        
        // Search in company
        if (user.comp && user.comp.toLowerCase().includes(searchLower)) {
          return true;
        }
        
        // Search in email
        if (user.cem && user.cem.toLowerCase().includes(searchLower)) {
          return true;
        }
        
        return false;
      });
    }
    
    // Apply role filter
    if (role) {
      filteredUsers = filteredUsers.filter(user => user.r === role);
    }
    
    // Sort users by username
    filteredUsers.sort((a, b) => {
      if (a.nm && b.nm) {
        return a.nm.localeCompare(b.nm);
      }
      return 0;
    });
    
    // Apply pagination
    const startIndex = (page - 1) * limit;
    const endIndex = page * limit;
    const paginatedUsers = filteredUsers.slice(startIndex, endIndex);
    
    // Format users for response (remove sensitive data)
    const formattedUsers = paginatedUsers.map(user => formatUser(user, false));
    
    // Return paginated results
    return res.status(200).json({
      users: formattedUsers,
      pagination: {
        total: filteredUsers.length,
        page,
        limit,
        pages: Math.ceil(filteredUsers.length / limit)
      }
    });
  } catch (error) {
    logger.error(`Error getting users: ${error.message}`);
    next(error);
  }
}

/**
 * Get user by ID
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function getUserById(req, res, next) {
  try {
    const { userId } = req.params;
    
    // Check if requester has admin privileges or is requesting their own profile
    if (!req.user || (req.user.a !== 'a' && req.user.u !== userId)) {
      return next(createForbiddenError('Insufficient permissions'));
    }
    
    // Get user from storage
    const user = global.storageService.getItem('users', userId);
    
    if (!user) {
      return next(createNotFoundError(`User with ID ${userId} not found`));
    }
    
    // Format user for response (include sensitive data only for admin or self)
    const includeSensitive = req.user.a === 'a' || req.user.u === userId;
    const formattedUser = formatUser(user, includeSensitive);
    
    // Return user
    return res.status(200).json({
      user: formattedUser
    });
  } catch (error) {
    logger.error(`Error getting user by ID: ${error.message}`);
    next(error);
  }
}

/**
 * Create a new user (Admin only)
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function createUser(req, res, next) {
  try {
    // Check if requester has admin privileges
    if (!req.user || req.user.a !== 'a') {
      return next(createForbiddenError('Admin privileges required'));
    }
    
    const { username, password, email, role, company, phone, street, postalCode, city, country } = req.body;
    
    // Validate required fields
    if (!username || !password || !email) {
      return next(createBadRequestError('Username, password, and email are required'));
    }
    
    // Check if username already exists
    const usersCollection = global.storageService.getCollection('users');
    const existingUser = Array.from(usersCollection.values()).find(
      user => user.nm && user.nm.toLowerCase() === username.toLowerCase()
    );
    
    if (existingUser) {
      return next(createBadRequestError('Username already exists'));
    }
    
    // Generate a new serial number for the user
    const serialNumber = `u${global.storageService.generateSerialNumber('u').substring(1)}`;
    
    // Create user model
    const User = require('../models/user');
    const newUser = new User(serialNumber);
    
    // Set user properties
    newUser.nm = username;
    
    // Generate password hash
    const salt = crypto.randomBytes(16).toString('hex');
    const hash = crypto.pbkdf2Sync(password, salt, 1000, 64, 'sha512').toString('hex');
    newUser.pwd = `${salt}:${hash}`;
    
    newUser.cem = email;
    newUser.r = role || 'u'; // Default role is 'u' (user)
    
    if (company) newUser.comp = company;
    if (phone) newUser.ph = phone;
    if (street) newUser.str = street;
    if (postalCode) newUser.zip = postalCode;
    if (city) newUser.cit = city;
    if (country) newUser.ctry = country;
    
    // Save to storage
    const saved = global.storageService.saveItem('users', serialNumber, newUser);
    
    if (!saved) {
      return next(createBadRequestError('Failed to create user'));
    }
    
    // Format user for response (remove sensitive data)
    const formattedUser = formatUser(newUser, false);
    
    // Return created user
    return res.status(201).json({
      message: 'User created successfully',
      user: formattedUser
    });
  } catch (error) {
    logger.error(`Error creating user: ${error.message}`);
    next(error);
  }
}

/**
 * Update a user
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function updateUser(req, res, next) {
  try {
    const { userId } = req.params;
    const { email, company, phone, street, postalCode, city, country, role } = req.body;
    
    // Check if requester has admin privileges or is updating their own profile
    const isAdmin = req.user && req.user.a === 'a';
    const isSelf = req.user && req.user.u === userId;
    
    if (!isAdmin && !isSelf) {
      return next(createForbiddenError('Insufficient permissions'));
    }
    
    // Get user from storage
    const user = global.storageService.getItem('users', userId);
    
    if (!user) {
      return next(createNotFoundError(`User with ID ${userId} not found`));
    }
    
    // Update fields if provided (role can only be updated by admin)
    if (email) user.cem = email;
    if (company) user.comp = company;
    if (phone) user.ph = phone;
    if (street) user.str = street;
    if (postalCode) user.zip = postalCode;
    if (city) user.cit = city;
    if (country) user.ctry = country;
    
    if (role && isAdmin) {
      user.r = role;
    }
    
    // Save to storage
    const saved = global.storageService.saveItem('users', userId, user);
    
    if (!saved) {
      return next(createBadRequestError('Failed to update user'));
    }
    
    // Format user for response
    const formattedUser = formatUser(user, isAdmin || isSelf);
    
    // Return updated user
    return res.status(200).json({
      message: 'User updated successfully',
      user: formattedUser
    });
  } catch (error) {
    logger.error(`Error updating user: ${error.message}`);
    next(error);
  }
}

/**
 * Change user password
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function changePassword(req, res, next) {
  try {
    const { userId } = req.params;
    const { currentPassword, newPassword } = req.body;
    
    // Check if requester has admin privileges or is changing their own password
    const isAdmin = req.user && req.user.a === 'a';
    const isSelf = req.user && req.user.u === userId;
    
    if (!isAdmin && !isSelf) {
      return next(createForbiddenError('Insufficient permissions'));
    }
    
    // Get user from storage
    const user = global.storageService.getItem('users', userId);
    
    if (!user) {
      return next(createNotFoundError(`User with ID ${userId} not found`));
    }
    
    // Verify current password (not required for admin)
    if (isSelf && !isAdmin) {
      if (!currentPassword) {
        return next(createBadRequestError('Current password is required'));
      }
      
      if (!user.pwd) {
        return next(createUnauthorizedError('Invalid credentials'));
      }
      
      const [salt, storedHash] = user.pwd.split(':');
      const hash = crypto.pbkdf2Sync(currentPassword, salt, 1000, 64, 'sha512').toString('hex');
      
      if (hash !== storedHash) {
        return next(createUnauthorizedError('Current password is incorrect'));
      }
    }
    
    // Validate new password
    if (!newPassword || newPassword.length < 8) {
      return next(createBadRequestError('New password must be at least 8 characters long'));
    }
    
    // Generate new password hash
    const salt = crypto.randomBytes(16).toString('hex');
    const hash = crypto.pbkdf2Sync(newPassword, salt, 1000, 64, 'sha512').toString('hex');
    user.pwd = `${salt}:${hash}`;
    
    // Save to storage
    const saved = global.storageService.saveItem('users', userId, user);
    
    if (!saved) {
      return next(createBadRequestError('Failed to change password'));
    }
    
    // Return success
    return res.status(200).json({
      message: 'Password changed successfully'
    });
  } catch (error) {
    logger.error(`Error changing password: ${error.message}`);
    next(error);
  }
}

/**
 * Delete a user (Admin only)
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function deleteUser(req, res, next) {
  try {
    const { userId } = req.params;
    
    // Check if requester has admin privileges
    if (!req.user || req.user.a !== 'a') {
      return next(createForbiddenError('Admin privileges required'));
    }
    
    // Get user from storage
    const user = global.storageService.getItem('users', userId);
    
    if (!user) {
      return next(createNotFoundError(`User with ID ${userId} not found`));
    }
    
    // Delete user
    user.active = false; // Soft delete
    
    // Save to storage
    const saved = global.storageService.saveItem('users', userId, user);
    
    if (!saved) {
      return next(createBadRequestError('Failed to delete user'));
    }
    
    // Return success
    return res.status(200).json({
      message: 'User deleted successfully'
    });
  } catch (error) {
    logger.error(`Error deleting user: ${error.message}`);
    next(error);
  }
}

/**
 * User login
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function login(req, res, next) {
  try {
    const { username, password } = req.body;
    
    // Validate required fields
    if (!username || !password) {
      return next(createBadRequestError('Username and password are required'));
    }
    
    // Find user by username
    const usersCollection = global.storageService.getCollection('users');
    const user = Array.from(usersCollection.values()).find(
      user => user.nm && user.nm.toLowerCase() === username.toLowerCase()
    );
    
    if (!user) {
      return next(createUnauthorizedError('Invalid credentials'));
    }
    
    // Check if user is active
    if (user.active === false) {
      return next(createUnauthorizedError('User account is inactive'));
    }
    
    // Verify password
    if (!user.pwd) {
      return next(createUnauthorizedError('Invalid credentials'));
    }
    
    const [salt, storedHash] = user.pwd.split(':');
    const hash = crypto.pbkdf2Sync(password, salt, 1000, 64, 'sha512').toString('hex');
    
    if (hash !== storedHash) {
      return next(createUnauthorizedError('Invalid credentials'));
    }
    
    // Generate JWT token
    const token = authService.createUserToken(user.sn[0], user.r || 'u');
    
    // Update last login timestamp
    user.lastLogin = Math.floor(Date.now() / 1000);
    global.storageService.saveItem('users', user.sn[0], user);
    
    // Format user for response
    const formattedUser = formatUser(user, true);
    
    // Return token and user data
    return res.status(200).json({
      message: 'Login successful',
      token,
      user: formattedUser
    });
  } catch (error) {
    logger.error(`Error during login: ${error.message}`);
    next(error);
  }
}

/**
 * Get current user profile
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function getCurrentUser(req, res, next) {
  try {
    if (!req.user || !req.user.u) {
      return next(createUnauthorizedError('Not authenticated'));
    }
    
    // Get user from storage
    const user = global.storageService.getItem('users', req.user.u);
    
    if (!user) {
      return next(createNotFoundError('User not found'));
    }
    
    // Format user for response
    const formattedUser = formatUser(user, true);
    
    // Return user
    return res.status(200).json({
      user: formattedUser
    });
  } catch (error) {
    logger.error(`Error getting current user: ${error.message}`);
    next(error);
  }
}

/**
 * Format user for API response
 * @param {Object} user - Raw user object
 * @param {boolean} includeSensitive - Whether to include sensitive data
 * @returns {Object} Formatted user
 */
function formatUser(user, includeSensitive = false) {
  // Create a deep copy of the user
  const formatted = JSON.parse(JSON.stringify(user));
  
  // Extract ID and creation timestamp
  if (formatted.sn && Array.isArray(formatted.sn)) {
    formatted.id = formatted.sn[0];
    if (formatted.sn.length > 1) {
      formatted.created_at = new Date(formatted.sn[1] * 1000);
    }
    delete formatted.sn;
  }
  
  // Format fields with meaningful names
  if (formatted.nm) {
    formatted.username = formatted.nm;
    delete formatted.nm;
  }
  
  if (formatted.r) {
    formatted.role = formatted.r;
    delete formatted.r;
  }
  
  if (formatted.comp) {
    formatted.company = formatted.comp;
    delete formatted.comp;
  }
  
  if (formatted.cem) {
    formatted.email = formatted.cem;
    delete formatted.cem;
  }
  
  if (formatted.ph) {
    formatted.phone = formatted.ph;
    delete formatted.ph;
  }
  
  if (formatted.str) {
    formatted.street = formatted.str;
    delete formatted.str;
  }
  
  if (formatted.zip) {
    formatted.postal_code = formatted.zip;
    delete formatted.zip;
  }
  
  if (formatted.cit) {
    formatted.city = formatted.cit;
    delete formatted.cit;
  }
  
  if (formatted.ctry) {
    formatted.country = formatted.ctry;
    delete formatted.ctry;
  }
  
  // Remove sensitive fields unless explicitly included
  if (!includeSensitive) {
    delete formatted.pwd; // Password hash
    delete formatted.token; // API tokens
    delete formatted.lastLogin; // Last login timestamp
  } else if (formatted.lastLogin) {
    formatted.last_login = new Date(formatted.lastLogin * 1000);
    delete formatted.lastLogin;
  }
  
  return formatted;
}

module.exports = {
  getAllUsers,
  getUserById,
  createUser,
  updateUser,
  changePassword,
  deleteUser,
  login,
  getCurrentUser
};