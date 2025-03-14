// middleware/auth.js
const passport = require('passport');
const jwt = require('jsonwebtoken');

// JWT Authentication middleware for protected routes
exports.requireAuth = passport.authenticate('jwt', { session: false });

// Optional authentication - doesn't block if not authenticated
exports.optionalAuth = (req, res, next) => {
  passport.authenticate('jwt', { session: false }, (err, user) => {
    if (user) req.user = user;
    next();
  })(req, res, next);
};

// Generate access and refresh tokens
exports.generateTokens = (user) => {
  const accessToken = jwt.sign(
    { userId: user.id, email: user.email, role: user.role },
    process.env.JWT_SECRET,
    { expiresIn: '1h' }
  );
  
  const refreshToken = jwt.sign(
    { userId: user.id },
    process.env.JWT_SECRET,
    { expiresIn: '7d' }
  );
  
  return { accessToken, refreshToken };
};

// Refresh token middleware
exports.refreshToken = async (req, res) => {
  try {
    const { refreshToken } = req.body;
    
    if (!refreshToken) {
      return res.status(400).json({ error: 'Refresh token required' });
    }
    
    const payload = jwt.verify(refreshToken, process.env.JWT_SECRET);
    const { UserAuth } = require('../models/postgresql');
    const user = await UserAuth.findByPk(payload.userId);
    
    if (!user) {
      return res.status(404).json({ error: 'User not found' });
    }
    
    // Generate new tokens
    const tokens = exports.generateTokens(user);
    
    res.json(tokens);
  } catch (error) {
    if (error.name === 'TokenExpiredError') {
      return res.status(401).json({ error: 'Refresh token expired' });
    }
    res.status(401).json({ error: 'Invalid refresh token' });
  }
};

// Admin authorization middleware
exports.requireAdmin = (req, res, next) => {
  if (req.user && req.user.role === 'admin') {
    return next();
  }
  res.status(403).json({ error: 'Admin access required' });
};

// Check if user owns the resource or is admin
exports.requireOwnershipOrAdmin = (resourceField) => {
  return (req, res, next) => {
    if (req.user.role === 'admin') {
      return next();
    }
    
    if (req[resourceField] && req[resourceField].userId === req.user.id) {
      return next();
    }
    
    res.status(403).json({ error: 'You do not have permission to access this resource' });
  };
};