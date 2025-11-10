// middleware/index.js
const { verifyJWT } = require('../../../shared/utils/encryption');

/**
 * Middleware to check if a user is authenticated
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object 
 * @param {Function} next - Express next function
 */
const isAuthenticated = (req, res, next) => {
    try {
        const token = req.headers.authorization?.split(' ')[1];
        
        if (!token) {
            return res.status(401).json({ error: 'Authentication required' });
        }
        
        req.user = verifyJWT(token, global.secretJwt);
        next();
    } catch (error) {
        return res.status(403).json({ error: 'Invalid or expired token' });
    }
};

/**
 * Middleware to check if a user is an admin
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
const isAdmin = (req, res, next) => {
    try {
        const token = req.headers.authorization?.split(' ')[1];
        
        if (!token) {
            return res.status(401).json({ error: 'Authentication required' });
        }
        
        const user = verifyJWT(token, global.secretJwt);
        
        if (user.a !== 'p') {
            return res.status(403).json({ error: 'Admin access required' });
        }
        
        req.user = user;
        next();
    } catch (error) {
        return res.status(403).json({ error: 'Invalid or expired token' });
    }
};

/**
 * Middleware to handle errors
 * @param {Error} err - Error object
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
const errorHandler = (err, req, res, next) => {
    console.error('Error:', err.message);
    res.status(err.status || 500).json({
        error: {
            message: err.message || 'Internal Server Error',
            status: err.status || 500
        }
    });
};

/**
 * Middleware to log requests
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
const requestLogger = (req, res, next) => {
    const start = Date.now(); // Сохраняем время начала запроса
    res.on('finish', () => {
        const ms = Date.now() - start; // Вычисляем разницу времени
        const language = req.i18n ? req.i18n.language : 'unknown';
        console.log(`${req.method} ${req.originalUrl} ${res.statusCode} ${ms}ms ${language}`);
    });
    next();
};

module.exports = {
    isAuthenticated,
    isAdmin,
    errorHandler,
    requestLogger
};