// routes/rma.js
const express = require('express');
const router = express.Router();
const path = require('path');
const { generateJWT, eckUrlEncrypt, eckCrc } = require('../../../shared/utils/encryption');
const { splitStreetAndHouseNumber, splitPostalCodeAndCity, convertToSerialDescriptionArray } = require('../utils/formatUtils');
const { generatePdfRma } = require('../utils/pdfGeneratorNew');
const { writeLargeMapToFile } = require('../utils/fileUtils');
const { resolve } = require('path');
const fs = require('fs');
const { optionalAuth, requireAuth } = require('../middleware/auth');
const { UserAuth, RmaRequest } = require('../../../shared/models/postgresql');
const { createRmaRequest } = require('../services/rmaService');

// SECURITY FIX: Helper function to escape HTML and prevent XSS
function escapeHtml(unsafe) {
  if (unsafe === null || unsafe === undefined) return '';
  return String(unsafe)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#039;");
}

// Apply optional authentication to all RMA routes
router.use(optionalAuth);

// Маршрут для создания RMA
router.post('/create', optionalAuth, async (req, res) => {
  try {
    const { 
      company, person, street, houseNumber, postalCode, city, country, 
      email, invoiceEmail, phone, resellerName, devices 
    } = req.body;
    
    // Создание RMA через сервисную функцию
    const newRma = await createRmaRequest({
      userId: req.user?.id,
      company,
      person,
      street,
      houseNumber,
      postalCode,
      city,
      country,
      email,
      invoiceEmail,
      phone,
      resellerName,
      devices
    });
    
    res.status(201).json({
      success: true,
      rmaCode: newRma.rmaCode,
      message: 'RMA request created successfully'
    });
    
  } catch (error) {
    console.error('Error creating RMA:', error);
    res.status(500).json({ error: error.message });
  }
});



// Check RMA status (allow unregistered users to check by RMA ID)
router.get('/status/:rmaId', async (req, res) => {
  try {
  const rmaId = req.params.rmaId;
    
    // Check in PostgreSQL first
    const rmaRequest = await RmaRequest.findOne({
      where: { rmaCode: rmaId }
    });
    
    if (rmaRequest) {
      // If user is logged in and owns this RMA or is admin, show detailed info
      if (req.user && (req.user.id === rmaRequest.userId || req.user.role === 'admin')) {
        return res.json(rmaRequest);
      }
      
      // Otherwise, show limited info (public view)
      return res.json({
        rmaCode: rmaRequest.rmaCode,
        status: rmaRequest.status,
        createdAt: rmaRequest.createdAt,
        receivedAt: rmaRequest.receivedAt,
        processedAt: rmaRequest.processedAt,
        shippedAt: rmaRequest.shippedAt,
        trackingNumber: rmaRequest.trackingNumber
      });
    }
    
    // Legacy fallback - check in the old system
  const betCode = 'o000' + rmaId;
    if (global.orders.has(betCode)) {
      const order = global.orders.get(betCode);
      
      return res.json({
        rmaCode: rmaId,
        status: 'created', // Default status in legacy system
        createdAt: new Date(order.sn[1] * 1000),
        company: order.comp,
        declarations: order.decl || []
      });
    }
    
    res.status(404).json({ error: 'RMA not found' });
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

// Get user's RMA requests
router.get('/my-requests', requireAuth, async (req, res) => {
  try {
    const rmaRequests = await RmaRequest.findAll({
      where: { userId: req.user.id },
      order: [['createdAt', 'DESC']]
    });
    
    res.json(rmaRequests);
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

// Link RMA request to user account (for temporary users)
router.post('/link-account', requireAuth, async (req, res) => {
  try {
    const { rmaCode, email } = req.body;
    
    if (!rmaCode || !email) {
      return res.status(400).json({ error: 'RMA code and email are required' });
    }
    
    // Find RMA by code and email
    const rmaRequest = await RmaRequest.findOne({
      where: { 
        rmaCode: rmaCode,
        email: email
      }
    });
    
    if (!rmaRequest) {
      return res.status(404).json({ error: 'RMA not found or email does not match' });
    }
    
    // Update RMA to link it to the current user
    rmaRequest.userId = req.user.id;
    await rmaRequest.save();
    
    res.json({ success: true, message: 'RMA linked to your account successfully' });
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

module.exports = router;