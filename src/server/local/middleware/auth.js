// middleware/auth.js
const passport = require('passport');
const jwt = require('jsonwebtoken');
const { UserAuth } = require('../../../shared/models/postgresql');
const { Buffer } = require('node:buffer');

// JWT Secret Key (read once and prepare for signing)
const jwtSecretBuffer = Buffer.from(process.env.JWT_SECRET, 'hex');

// JWT Authentication middleware for protected routes
exports.requireAuth = passport.authenticate('jwt', { session: false });

// Optional authentication - doesn't block if not authenticated
exports.optionalAuth = (req, res, next) => {
  passport.authenticate('jwt', { session: false }, (err, user) => {
    if (user) req.user = user;
    next();
  })(req, res, next);
};

// Generate access and refresh tokens using the 'jsonwebtoken' library
exports.generateTokens = (user) => {
  const accessToken = jwt.sign(
    {
      id: user.id, // Use 'id' to match passport's expectation
      email: user.email,
      role: user.role,
      userType: user.userType
    },
    jwtSecretBuffer,
    { expiresIn: '1h' }
  );

  const refreshToken = jwt.sign(
    { id: user.id },
    jwtSecretBuffer,
    { expiresIn: '90d' }
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

    const payload = jwt.verify(refreshToken, jwtSecretBuffer);

    const user = await UserAuth.findByPk(payload.id);

    if (!user) {
      return res.status(404).json({ error: 'User not found' });
    }

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
  passport.authenticate('jwt', { session: false }, (err, user, info) => {
    if (err) {
      return next(err);
    }

    if (!user) {
      return res.status(401).json({ error: 'Authentication required' });
    }

    if (user.role !== 'admin') {
      return res.status(403).json({ error: 'Admin access required' });
    }

    req.user = user;
    next();
  })(req, res, next);
};

// Company user authorization middleware
exports.requireCompany = (req, res, next) => {
  passport.authenticate('jwt', { session: false }, (err, user, info) => {
    if (err) {
      return next(err);
    }

    if (!user) {
      return res.status(401).json({ error: 'Authentication required' });
    }

    if (user.userType !== 'company' && user.role !== 'admin') {
      return res.status(403).json({ error: 'Company access required' });
    }

    req.user = user;
    next();
  })(req, res, next);
};

// Check if user owns the resource or is admin
exports.requireOwnershipOrAdmin = (resourceField) => {
  return (req, res, next) => {
    if (!req.user) {
      return res.status(401).json({ error: 'Authentication required' });
    }

    if (req.user.role === 'admin') {
      return next();
    }

    if (req[resourceField] && req[resourceField].userId === req.user.id) {
      return next();
    }

    res.status(403).json({ error: 'You do not have permission to access this resource' });
  };
};

// Admin authorization for HTML pages - redirects to login instead of returning JSON
exports.requireAdminPage = (req, res, next) => {
  passport.authenticate('jwt', { session: false }, (err, user, info) => {
    if (err) {
      return next(err);
    }

    if (!user) {
      // Redirect to login with return URL
      const returnUrl = encodeURIComponent(req.originalUrl);
      return res.redirect(`/auth/login?redirect=${returnUrl}`);
    }

    if (user.role !== 'admin') {
      // For non-admins, also redirect to login (they'll see error there)
      return res.redirect('/auth/login?error=admin_required');
    }

    req.user = user;
    next();
  })(req, res, next);
};
