// routes/index.js
const express = require('express');
const router = express.Router();

// Import route modules
const itemRoutes = require('./itemRoutes');
const boxRoutes = require('./boxRoutes');
const placeRoutes = require('./placeRoutes');
const orderRoutes = require('./orderRoutes');
const userRoutes = require('./userRoutes');
const rmaRoutes = require('./rmaRoutes');
const authRoutes = require('./authRoutes');

// Mount routes
router.use('/api/items', itemRoutes);
router.use('/api/boxes', boxRoutes);
router.use('/api/places', placeRoutes);
router.use('/api/orders', orderRoutes);
router.use('/api/users', userRoutes);
router.use('/api/rma', rmaRoutes);
router.use('/api/auth', authRoutes);

// Health check endpoint
router.get('/api/health', (req, res) => {
  res.status(200).json({ 
    status: 'ok',
    version: process.env.npm_package_version || '1.0.0',
    timestamp: new Date().toISOString()
  });
});

module.exports = router;