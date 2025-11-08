// routes/rma.js
const express = require('express');
const router = express.Router();
const path = require('path');
const { generateJWT, betrugerUrlEncrypt, betrugerCrc } = require('../utils/encryption');
const { splitStreetAndHouseNumber, splitPostalCodeAndCity, convertToSerialDescriptionArray } = require('../utils/formatUtils');
const { generatePdfRma } = require('../utils/pdfGenerator');
const { writeLargeMapToFile } = require('../utils/fileUtils');
const { resolve } = require('path');
const fs = require('fs');
const { optionalAuth, requireAuth } = require('../middleware/auth');
const { UserAuth, RmaRequest } = require('../../shared/models/postgresql');
const { createRmaRequest } = require('../services/rmaService');

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






// Generate RMA form page (HTML version)
router.post('/generate', (req, res) => {
  const timestamp = Math.floor(Date.now() / 1000);
  const rmaCode = `RMA${timestamp}${betrugerCrc(timestamp)}`;

  const payload = {
    r: rmaCode,
    a: 'p',
    e: Math.floor(Date.now() / 1000) + 60 * 60 * 24 * 90 // Expire in 3 months
  };

  const token = generateJWT(payload, global.secretJwt);

  // Get user data if logged in
  let userData = { 
                  company: '',
    vat: '',
                  person: '',
                  street: '',
    houseNumber: '',
    addressLine2: '',
    city: '', 
                  postalCode: '',
                  country: '',
                  email: '',
    phone: '' 
  };

  if (req.user) {
    userData = {
      company: req.user.company || '',
      vat: req.user.vat || '',
      person: req.user.name || '',
      street: req.user.street || '',
      houseNumber: req.user.houseNumber || '',
      addressLine2: req.user.addressLine2 || '',
      city: req.user.city || '',
      postalCode: req.user.postalCode || '',
      country: req.user.country || '',
      email: req.user.email || '',
      phone: req.user.phone || ''
    };
  }

  res.send(`
   <span class="text3">
<div style="font-family: Arial, sans-serif; max-width: 600px; margin: 20px auto;">
  <form id="rmaForm" onsubmit="return myFetch('formSubmit', 'rmaForm', 'pdfRma','${token}','/rma/confirm');">
    <input type="text" id="rma" value="${rmaCode}" readonly required 
           style="font-size: 20px; width: 95%; padding: 5px; background-color: #eee; margin-top: 5px;">
    
    <!-- Billing Information Section -->
    <div style="margin-top: 20px; border: 1px solid #ddd; padding: 15px; border-radius: 4px;">
      <h3 style="margin-top: 0; color: #1e2071;" data-i18n="rma:form.billing_info">Billing Information</h3>
      
      <label for="company" style="display: block; margin-top: 10px;"><b data-i18n="rma:form.company_name">Company Name:</b></label>
      <input type="text" id="company" value="${userData.company}" required 
             style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

      <label for="vat" style="display: block; margin-top: 10px;"><span data-i18n="rma:form.vat_number">VAT Number:</span></label>
      <input type="text" id="vat" value="${userData.vat}"
             style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

      <label for="person" style="display: block; margin-top: 10px;"><span data-i18n="rma:form.contact_person">Contact Person:</span></label>
      <input type="text" id="person" value="${userData.person}"
             style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

      <label for="street" style="display: block; margin-top: 10px;"><b data-i18n="rma:form.street">Street:</b></label>
      <input type="text" id="street" value="${userData.street}" required 
             style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

      <label for="houseNumber" style="display: block; margin-top: 10px;"><b data-i18n="rma:form.house_number">House Number:</b></label>
      <input type="text" id="houseNumber" value="${userData.houseNumber}" required 
             style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

      <label for="addressLine2" style="display: block; margin-top: 10px;"><span data-i18n="rma:form.additional_address">Additional Address Line:</span></label>
      <input type="text" id="addressLine2" value="${userData.addressLine2}" 
             style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

      <label for="postalCode" style="display: block; margin-top: 10px;"><b data-i18n="rma:form.postal_code">Postal Code:</b></label>
      <input type="text" id="postalCode" value="${userData.postalCode}" required 
             style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

      <label for="city" style="display: block; margin-top: 10px;"><b data-i18n="rma:form.city">City:</b></label>
      <input type="text" id="city" value="${userData.city}" required 
             style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

      <label for="country" style="display: block; margin-top: 10px;"><b data-i18n="rma:form.country">Country:</b></label>
      <input type="text" id="country" value="${userData.country}" required 
             style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">
                      </div>
                      
    <!-- Contact Information Section -->
    <div style="margin-top: 20px; border: 1px solid #ddd; padding: 15px; border-radius: 4px;">
      <h3 style="margin-top: 0; color: #1e2071;" data-i18n="rma:form.contact_info">Contact Information</h3>
      
      <label for="email" style="display: block; margin-top: 10px;"><b data-i18n="rma:form.contact_email">Contact Email:</b></label>
      <input type="email" id="email" value="${userData.email}" required 
             style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

      <label for="invoice_email" style="display: block; margin-top: 10px;"><span data-i18n="rma:form.invoice_email">E-Invoice Email:</span></label>
      <input type="email" id="invoice_email" 
             style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

      <label for="phone" style="display: block; margin-top: 10px;"><span data-i18n="rma:form.phone">Phone:</span></label>
      <input type="tel" id="phone" value="${userData.phone}"
             style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

      <label for="resellerName" style="display: block; margin-top: 10px;"><span data-i18n="rma:form.reseller_name">In case of warranty, please provide the reseller's name:</span></label>
      <input type="text" id="resellerName" 
             style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">
                  </div>
                  
    <!-- Hidden user ID if logged in -->
    ${req.user ? `<input type="hidden" id="userId" value="${req.user.id}">` : ''}

    <!-- Devices Section -->
    <div style="margin-top: 20px;">
      <h3 style="color: #1e2071;" data-i18n="rma:form.device_info">Device Information</h3>
      <p style="margin-bottom: 20px; font-style: italic;" data-i18n="rma:device.info_text">Add devices that need to be repaired. Each device can be shipped to a different address if needed.</p>
      
      <div id="devices-container">
        <!-- Initial device entry will be added by JavaScript -->
                        </div>
                        
      <div style="margin-top: 20px; text-align: center;">
        <button type="button" id="add-device-btn" style="background-color: #1e2071; color: white; border: none; padding: 8px 15px; border-radius: 4px; cursor: pointer;" data-i18n="rma:form.add_device">
                      Add Another Device
                    </button>
                  </div>
                </div>
                
    <!-- Submit Button -->
    <div style="margin-top: 30px; display: flex; justify-content: space-between;">
      <button class="buttonFlat" type="button" onclick="location.reload()" 
        style="font-size: 20px; padding: 10px 20px; background-color: #1e2071; border: none; border-radius: 4px; cursor: pointer;" data-i18n="rma:form.back"> 
                    Back
                  </button>
                  
      <button class="buttonFlat" type="submit" 
        style="font-size: 20px; padding: 10px 20px; background-color: #1e2071; color: white; border: none; border-radius: 4px; cursor: pointer;" data-i18n="rma:form.submit"> 
        Submit Form
                  </button>
                </div>
              </form>
            </div>

<script>
  // Load rma-form.js for dynamic device entries
  lazyLoadScript('/js/rma-form.js');
  
  // Ensure translations are applied after the page loads
  document.addEventListener('DOMContentLoaded', function() {
    if (window.i18n && window.i18n.getCurrentLanguage() !== 'en') {
      window.i18n.updatePageTranslations();
    }
  });
      </script>
  `);
});

// Submit RMA form
router.post('/confirm', async (req, res) => {
  try {
    const rmaJson = req.body;
    console.log(rmaJson);
    // Generate tokens for tracking and full access
    const payload1 = {
      r: rmaJson.rma.trim(),
      a: 'l',
      e: Math.floor(Date.now() / 1000) + 60 * 60 * 24 * 30 // Expire in a month
    };
    
    const payload2 = {
      r: rmaJson.rma.trim(),
      a: 'p',
      e: Math.floor(Date.now() / 1000) + 60 * 60 * 24 * 90 // Expire in 3 months
    };

    const token1 = generateJWT(payload1, global.secretJwt);
    const token2 = generateJWT(payload2, global.secretJwt);
    const linkToken = `https://m3.repair/jwt/${token1}`;

    // Format and validate input
    let formattedInput = rmaJson.rma.trim();
    if (formattedInput.length > 18) {
      throw new Error("Input value is too long");
    }
    
    formattedInput = 'o' + formattedInput.padStart(18, '0');
    
    // Generate PDF
    const pdfBuffer = await generatePdfRma(rmaJson, linkToken, token2, betrugerUrlEncrypt(formattedInput, process.env.ENC_KEY));

    // Create order in both systems
    await createOrderFromRma(formattedInput, rmaJson, req.user?.id || rmaJson.userId);
    
    // Send PDF
    res.writeHead(200, {
      'Content-Type': 'application/pdf',
      'Content-Disposition': 'attachment; filename="rma.pdf"',
      'Content-Length': pdfBuffer.length
    });
    
    res.end(pdfBuffer);
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

// Helper function to create order from RMA data (in both systems)
async function createOrderFromRma(formattedInput, rmaJson, userId = null) {
  // Legacy system - Create order object
  const tempObj = Object.create(global.order);
  tempObj.sn = [formattedInput, Math.floor(Date.now() / 1000)];
  tempObj.cust = { 'reseller': rmaJson.resellerName };
  tempObj.comp = rmaJson.company;
  tempObj.pers = rmaJson.person;
  
  const addressInfo1 = splitStreetAndHouseNumber(rmaJson.street);
  tempObj.str = addressInfo1.street;
  tempObj.hs = addressInfo1.houseNumber;
  
  const addressInfo2 = splitPostalCodeAndCity(rmaJson.postal);
  tempObj.zip = addressInfo2.postalCode;
  tempObj.cit = addressInfo2.city;

  tempObj.ctry = rmaJson.country;
  tempObj.cem = rmaJson.email;
  tempObj.iem = rmaJson.invoice_email;
  tempObj.ph = rmaJson.phone;
  tempObj.cont = [];
  tempObj.decl = convertToSerialDescriptionArray(rmaJson);
  
  // Add to legacy system
  global.orders.set(formattedInput, tempObj);
  
  try {
    // Save to legacy file system
    await writeLargeMapToFile(global.orders, resolve(`${global.baseDirectory}base/orders.json`));
    
    // Convert device data for PostgreSQL
    const devices = [];
    for (let i = 1; i <= 30; i++) {
      if (rmaJson[`serial${i}`] && rmaJson[`description${i}`]) {
        devices.push({
          serialNumber: rmaJson[`serial${i}`],
          description: rmaJson[`description${i}`]
        });
      }
    }
    
    // Prepare RMA data for PostgreSQL
    const rmaData = {
      userId: userId, // Will be null if not logged in
      rmaCode: rmaJson.rma,
      orderCode: formattedInput,
      company: rmaJson.company,
      person: rmaJson.person || null,
      street: addressInfo1.street,
      houseNumber: addressInfo1.houseNumber || null,
      postalCode: addressInfo2.postalCode,
      city: addressInfo2.city,
      country: rmaJson.country,
      email: rmaJson.email,
      invoiceEmail: rmaJson.invoice_email || null,
      phone: rmaJson.phone || null,
      resellerName: rmaJson.resellerName || null,
      devices: devices,
      orderData: tempObj // Store original for compatibility
    };
    
    // If not logged in, check if user exists by email
    let user = null;
    if (!userId && rmaJson.email) {
      user = await UserAuth.findOne({ where: { email: rmaJson.email } });
      
      if (!user) {
        // Create temporary username based on email
        const emailParts = rmaJson.email.split('@');
        let tempUsername = emailParts[0];
        
        // Check if username exists
        const existingUsername = await UserAuth.findOne({ where: { username: tempUsername } });
        
        // If username exists, add random suffix
        if (existingUsername) {
          const randomSuffix = Math.floor(Math.random() * 10000);
          tempUsername = `${tempUsername}${randomSuffix}`;
        }
        
        // Determine if this is a company or individual
        const userType = rmaJson.company ? 'company' : 'individual';
        
        // Create RMA user account
        user = await UserAuth.create({
          username: tempUsername,
          email: rmaJson.email,
          name: rmaJson.person || '',
          company: rmaJson.company || '',
          phone: rmaJson.phone || '',
          street: addressInfo1.street || '',
          houseNumber: addressInfo1.houseNumber || '',
          postalCode: addressInfo2.postalCode || '',
          city: addressInfo2.city || '',
          country: rmaJson.country || '',
          role: 'rma',  // Mark as RMA user
          userType: userType,
          rmaReference: rmaJson.rma  // Store RMA reference
        });
        
        console.log(`Created new RMA user account for ${rmaJson.email} with RMA ${rmaJson.rma}`);
      } else if (user.role === 'rma') {
        // If already an RMA user, update the reference
        user.rmaReference = rmaJson.rma;
        await user.save();
      }
      
      // Set the userId for the RMA request
      rmaData.userId = user.id;
    }
    
    // Create in PostgreSQL
    await RmaRequest.create(rmaData);
    
    console.log(`RMA ${rmaJson.rma} created successfully in both systems`);
  } catch (err) {
    console.error('Error saving RMA:', err);
    throw err; // Re-throw to be caught by the route handler
  }
}

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