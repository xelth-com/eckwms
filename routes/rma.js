// routes/rma.js
const express = require('express');
const router = express.Router();
const path = require('path');
const passport = require('passport');
const { generateJWT, betrugerUrlEncrypt, betrugerCrc } = require('../utils/encryption');
const { splitStreetAndHouseNumber, splitPostalCodeAndCity, convertToSerialDescriptionArray } = require('../utils/formatUtils');
const { generatePdfRma } = require('../utils/pdfGenerator');
const { writeLargeMapToFile } = require('../utils/fileUtils');
const { resolve } = require('path');
const fs = require('fs');
const { optionalAuth, requireAuth } = require('../middleware/auth');
const { UserAuth, RmaRequest } = require('../models/postgresql');

// Apply optional authentication to all RMA routes
router.use(optionalAuth);

// Generate RMA form page (HTML version)
router.all('/generate', (req, res) => {
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
    person: '', 
    street: '', 
    city: '', 
    postalCode: '', 
    country: 'Germany',
    email: '', 
    phone: '' 
  };

  if (req.user) {
    userData = {
      company: req.user.company || '',
      person: req.user.name || '',
      street: req.user.street || '',
      city: req.user.city || '',
      postalCode: req.user.postalCode || '',
      country: req.user.country || 'Germany',
      email: req.user.email || '',
      phone: req.user.phone || ''
    };
  }

  res.send(`
   <span class="text3">
<div style="font-family: Arial, sans-serif; max-width: 600px; margin: 20px auto;">
  <h2>RMA Form</h2>
  ${req.user ? 
    `<div style="background-color: #e6f7ff; border: 1px solid #91d5ff; border-radius: 4px; padding: 10px; margin-bottom: 15px;">
      You are logged in as <strong>${req.user.email}</strong>. The form has been pre-filled with your information.
     </div>` 
    : 
    `<div style="background-color: #fff7e6; border: 1px solid #ffd591; border-radius: 4px; padding: 10px; margin-bottom: 15px;">
      Not logged in. <a href="/auth/login" style="color: #1890ff; text-decoration: underline;">Log in</a> or 
      <a href="/auth/register" style="color: #1890ff; text-decoration: underline;">register</a> to save your RMA details to your account.
     </div>`
  }
  <form id="rmaForm" onsubmit="return myFetch('formSubmit', 'rmaForm', 'pdfRma','${token}','/rma/confirm');">
    <input type="text" id="rma" value="${rmaCode}" readonly required 
           style="font-size: 20px; width: 95%; padding: 5px; background-color: #eee; margin-top: 5px;">
    <!-- Company Information -->
    <label for="company" style="display: block; margin-top: 10px;"><b>Company Name:</b></label>
    <input type="text" id="company" value="${userData.company}" required 
           style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

    <label for="person" style="display: block; margin-top: 10px;">Contact Person:</label>
    <input type="text" id="person" value="${userData.person}"
           style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

    <label for="street" style="display: block; margin-top: 10px;"><b>Street and House Number:</b></label>
    <input type="text" id="street" value="${userData.street}" required 
           style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

    <label for="postal" style="display: block; margin-top: 10px;"><b>Postal Code / City:</b></label>
    <input type="text" id="postal" value="${userData.postalCode ? userData.postalCode + ' ' + userData.city : ''}" required 
           style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

    <label for="country" style="display: block; margin-top: 10px;"><b>Country:</b></label>
    <input type="text" id="country" value="${userData.country}" required 
           style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

    <!-- Contact Information -->
    <label for="email" style="display: block; margin-top: 10px;"><b>Contact Email:</b></label>
    <input type="email" id="email" value="${userData.email}" required 
           style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

    <label for="invoice_email" style="display: block; margin-top: 10px;">E-Invoice Email:</label>
    <input type="email" id="invoice_email" 
           style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

    <label for="phone" style="display: block; margin-top: 10px;">Phone:</label>
    <input type="tel" id="phone" value="${userData.phone}"
           style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

    <label for="resellerName" style="display: block; margin-top: 10px;">In case of warranty, please provide the reseller's name:</label>
    <input type="text" id="resellerName" 
           style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

    <!-- Hidden user ID if logged in -->
    ${req.user ? `<input type="hidden" id="userId" value="${req.user.id}">` : ''}

    <!-- Seriennummern und Fehlerbeschreibungen -->
    <br><br><br><br>

    <div style="display: flex; flex-wrap: wrap; margin-top: 10px;">
        <div style="flex: 0 0 170px; margin-right: 10px;">
            <label for="serial1" style=" margin-top: 10px;">Serial Number 1:</label><br>
            <input type="text" id="serial1" style="width: 170px; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">
        </div>
        <div style="flex: 1 1 300px;">
            <label for="description1" style=" margin-top: 10px;">Issue Description:</label><br>
            <textarea id="description1" rows="3" style="width: 95%; min-width: 300px; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;"></textarea>
        </div>
    </div>
    <br>
    <div style="display: flex; flex-wrap: wrap; margin-top: 10px;">
        <div style="flex: 0 0 170px; margin-right: 10px;">
            <label for="serial2" style=" margin-top: 10px;">Serial Number 2:</label><br>
            <input type="text" id="serial2" style="width: 170px; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">
        </div>
        <div style="flex: 1 1 300px;">
            <label for="description2" style=" margin-top: 10px;">Issue Description:</label><br>
            <textarea id="description2" rows="3" style="width: 95%; min-width: 300px; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;"></textarea>
        </div>
    </div>
    <br>
    <div style="display: flex; flex-wrap: wrap; margin-top: 10px;">
        <div style="flex: 0 0 170px; margin-right: 10px;">
            <label for="serial3" style=" margin-top: 10px;">Serial Number 3:</label><br>
            <input type="text" id="serial3" style="width: 170px; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">
        </div>
        <div style="flex: 1 1 300px;">
            <label for="description3" style=" margin-top: 10px;">Issue Description:</label><br>
            <textarea id="description3" rows="3" style="width: 95%; min-width: 300px; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;"></textarea>
        </div>
    </div>
    <br>
    <div style="display: flex; flex-wrap: wrap; margin-top: 10px;">
        <div style="flex: 0 0 170px; margin-right: 10px;">
            <label for="serial4" style=" margin-top: 10px;">Serial Number 4:</label><br>
            <input type="text" id="serial4" style="width: 170px; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">
        </div>
        <div style="flex: 1 1 300px;">
            <label for="description4" style=" margin-top: 10px;">Issue Description:</label><br>
            <textarea id="description4" rows="3" style="width: 95%; min-width: 300px; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;"></textarea>
        </div>
    </div>
    <br>
    <div style="display: flex; flex-wrap: wrap; margin-top: 10px;">
        <div style="flex: 0 0 170px; margin-right: 10px;">
            <label for="serial5" style=" margin-top: 10px;">Serial Number 5:</label><br>
            <input type="text" id="serial5" style="width: 170px; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">
        </div>
        <div style="flex: 1 1 300px;">
            <label for="description5" style=" margin-top: 10px;">Issue Description:</label><br>
            <textarea id="description5" rows="3" style="width: 95%; min-width: 300px; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;"></textarea>
        </div>
    </div>
    <br>
    <!-- Submit Button -->
    <button class="buttonFlat" type="submit" style="font-size: 20px;margin: 5px; margin-left: max(calc((100% - 450px)/10),0px);float:left;"> 
      Submit Form
    </button> 
  </form>

  <button class="buttonFlat" type="" onclick="location.reload()" style="font-size: 20px; margin: 5px; margin-right: max(calc((100% - 450px)/10),0px);float:right;"> 
    Back
  </button> 
  <br><br><br>
</div>
  `);
});

// Submit RMA form
router.post('/confirm', async (req, res) => {
  try {
    const rmaJson = req.body;
    
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
router.post('/my-requests', requireAuth, async (req, res) => {
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