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
const { UserAuth, RmaRequest } = require('../models/postgresql');
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
                  country: 'Germany',
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
      country: req.user.country || 'Germany',
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
      <h3 style="margin-top: 0; color: #1e2071;">Billing Information</h3>
      
      <label for="company" style="display: block; margin-top: 10px;"><b>Company Name:</b></label>
      <input type="text" id="company" value="${userData.company}" required 
             style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

      <label for="vat" style="display: block; margin-top: 10px;">VAT Number:</label>
      <input type="text" id="vat" value="${userData.vat}"
             style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

      <label for="person" style="display: block; margin-top: 10px;">Contact Person:</label>
      <input type="text" id="person" value="${userData.person}"
             style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

      <label for="street" style="display: block; margin-top: 10px;"><b>Street:</b></label>
      <input type="text" id="street" value="${userData.street}" required 
             style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

      <label for="houseNumber" style="display: block; margin-top: 10px;"><b>House Number:</b></label>
      <input type="text" id="houseNumber" value="${userData.houseNumber}" required 
             style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

      <label for="addressLine2" style="display: block; margin-top: 10px;">Additional Address Line:</label>
      <input type="text" id="addressLine2" value="${userData.addressLine2}" 
             style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

      <label for="postalCode" style="display: block; margin-top: 10px;"><b>Postal Code:</b></label>
      <input type="text" id="postalCode" value="${userData.postalCode}" required 
             style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

      <label for="city" style="display: block; margin-top: 10px;"><b>City:</b></label>
      <input type="text" id="city" value="${userData.city}" required 
             style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

      <label for="country" style="display: block; margin-top: 10px;"><b>Country:</b></label>
      <input type="text" id="country" value="${userData.country}" required 
             style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">
                      </div>
                      
    <!-- Contact Information Section -->
    <div style="margin-top: 20px; border: 1px solid #ddd; padding: 15px; border-radius: 4px;">
      <h3 style="margin-top: 0; color: #1e2071;">Contact Information</h3>
      
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
                  </div>
                  
    <!-- Hidden user ID if logged in -->
    ${req.user ? `<input type="hidden" id="userId" value="${req.user.id}">` : ''}

    <!-- Devices Section -->
    <div style="margin-top: 20px;">
      <h3 style="color: #1e2071;">Device Information</h3>
      <p style="margin-bottom: 20px; font-style: italic;">Add devices that need to be repaired. Each device can be shipped to a different address if needed.</p>
      
      <div id="devices-container">
        <!-- Initial device entry will be added by JavaScript -->
                        </div>
                        
      <div style="margin-top: 20px; text-align: center;">
        <button type="button" id="add-device-btn" style="background-color: #1e2071; color: white; border: none; padding: 8px 15px; border-radius: 4px; cursor: pointer;">
                      Add Another Device
                    </button>
                  </div>
                </div>
                
    <!-- Submit Button -->
    <div style="margin-top: 30px; display: flex; justify-content: space-between;">
      <button class="buttonFlat" type="button" onclick="location.reload()" 
        style="font-size: 20px; padding: 10px 20px; background-color: #1e2071; border: none; border-radius: 4px; cursor: pointer;"> 
                    Back
                  </button>
                  
      <button class="buttonFlat" type="submit" 
        style="font-size: 20px; padding: 10px 20px; background-color: #1e2071; color: white; border: none; border-radius: 4px; cursor: pointer;"> 
        Submit Form
                  </button>
                </div>
              </form>
            </div>

<script>
  // Tracks the current device count
  let deviceCount = 0;
  
  // Tracks which return address to use for new devices
  let currentReturnAddressIndex = null;
  
  // Add initial device entry when page loads
  document.addEventListener('DOMContentLoaded', function() {
    addDeviceEntry();
    
    // Set up Add Device button
    document.getElementById('add-device-btn').addEventListener('click', function() {
      addDeviceEntry();
    });
  });
  
  // Function to add a new device entry
  function addDeviceEntry() {
    deviceCount++;
    const deviceIndex = deviceCount;
    
    const deviceEntry = document.createElement('div');
    deviceEntry.className = 'device-entry';
    deviceEntry.dataset.index = deviceIndex;
    deviceEntry.style.border = '1px solid #ddd';
    deviceEntry.style.borderRadius = '4px';
    deviceEntry.style.padding = '15px';
    deviceEntry.style.marginBottom = '20px';
    deviceEntry.style.backgroundColor = '#f9f9f9';
    
    // Create the device header with device number
    const deviceHeader = document.createElement('div');
    deviceHeader.style.display = 'flex';
    deviceHeader.style.justifyContent = 'space-between';
    deviceHeader.style.alignItems = 'center';
    deviceHeader.style.marginBottom = '15px';
    
    const deviceTitle = document.createElement('h4');
    deviceTitle.textContent = \`Device #\${deviceIndex}\`;
    deviceTitle.style.margin = '0';
    deviceTitle.style.color = '#1e2071';
    
    // Toggle button for alternate shipping address
    const toggleButton = document.createElement('button');
    toggleButton.type = 'button';
    toggleButton.textContent = 'Specify Different Return Address';
    toggleButton.style.backgroundColor = '#eee';
    toggleButton.style.border = '1px solid #ccc';
    toggleButton.style.borderRadius = '4px';
    toggleButton.style.padding = '5px 10px';
    toggleButton.style.cursor = 'pointer';
    toggleButton.dataset.deviceIndex = deviceIndex;
    
    deviceHeader.appendChild(deviceTitle);
    deviceHeader.appendChild(toggleButton);
    deviceEntry.appendChild(deviceHeader);
    
    // Create alternate shipping address section (initially hidden)
    const alternateAddressSection = document.createElement('div');
    alternateAddressSection.id = \`alternate-address-\${deviceIndex}\`;
    alternateAddressSection.style.display = 'none';
    alternateAddressSection.style.padding = '10px';
    alternateAddressSection.style.border = '1px solid #ddd';
    alternateAddressSection.style.borderRadius = '4px';
    alternateAddressSection.style.marginBottom = '15px';
    alternateAddressSection.style.backgroundColor = '#fff';
    
    // Add alternate address fields with the updated structure
    alternateAddressSection.innerHTML = \`
      <h4 style="margin-top: 0; color: #1e2071;">Alternate Return Address</h4>
      <p style="font-style: italic; margin-bottom: 10px;">Specify a different address for returning this device.</p>
      
      <label for="alt_company_\${deviceIndex}" style="display: block; margin-top: 10px;"><b>Company Name:</b></label>
      <input type="text" id="alt_company_\${deviceIndex}" name="alt_company_\${deviceIndex}" 
             style="width: 95%; padding: 5px; font-size: 16px; background-color: #eee; margin-top: 5px;">

      <label for="alt_vat_\${deviceIndex}" style="display: block; margin-top: 10px;">VAT Number:</label>
      <input type="text" id="alt_vat_\${deviceIndex}" name="alt_vat_\${deviceIndex}" 
             style="width: 95%; padding: 5px; font-size: 16px; background-color: #eee; margin-top: 5px;">

      <label for="alt_person_\${deviceIndex}" style="display: block; margin-top: 10px;">Contact Person:</label>
      <input type="text" id="alt_person_\${deviceIndex}" name="alt_person_\${deviceIndex}" 
             style="width: 95%; padding: 5px; font-size: 16px; background-color: #eee; margin-top: 5px;">

      <label for="alt_street_\${deviceIndex}" style="display: block; margin-top: 10px;"><b>Street:</b></label>
      <input type="text" id="alt_street_\${deviceIndex}" name="alt_street_\${deviceIndex}" 
             style="width: 95%; padding: 5px; font-size: 16px; background-color: #eee; margin-top: 5px;">

      <label for="alt_houseNumber_\${deviceIndex}" style="display: block; margin-top: 10px;"><b>House Number:</b></label>
      <input type="text" id="alt_houseNumber_\${deviceIndex}" name="alt_houseNumber_\${deviceIndex}" 
             style="width: 95%; padding: 5px; font-size: 16px; background-color: #eee; margin-top: 5px;">

      <label for="alt_addressLine2_\${deviceIndex}" style="display: block; margin-top: 10px;">Additional Address Line:</label>
      <input type="text" id="alt_addressLine2_\${deviceIndex}" name="alt_addressLine2_\${deviceIndex}" 
             style="width: 95%; padding: 5px; font-size: 16px; background-color: #eee; margin-top: 5px;">

      <label for="alt_postalCode_\${deviceIndex}" style="display: block; margin-top: 10px;"><b>Postal Code:</b></label>
      <input type="text" id="alt_postalCode_\${deviceIndex}" name="alt_postalCode_\${deviceIndex}" 
             style="width: 95%; padding: 5px; font-size: 16px; background-color: #eee; margin-top: 5px;">

      <label for="alt_city_\${deviceIndex}" style="display: block; margin-top: 10px;"><b>City:</b></label>
      <input type="text" id="alt_city_\${deviceIndex}" name="alt_city_\${deviceIndex}" 
             style="width: 95%; padding: 5px; font-size: 16px; background-color: #eee; margin-top: 5px;">

      <label for="alt_country_\${deviceIndex}" style="display: block; margin-top: 10px;"><b>Country:</b></label>
      <input type="text" id="alt_country_\${deviceIndex}" name="alt_country_\${deviceIndex}" value="Germany" 
             style="width: 95%; padding: 5px; font-size: 16px; background-color: #eee; margin-top: 5px;">
      
      <div style="margin-top: 10px;">
        <label>
          <input type="checkbox" id="use_for_all_\${deviceIndex}" name="use_for_all_\${deviceIndex}">
          Use this address for all subsequent devices
        </label>
      </div>
    \`;
    
    deviceEntry.appendChild(alternateAddressSection);
    
    // Create serial number and description section
    const deviceDetailsSection = document.createElement('div');
    
    // Serial number input
    const serialLabel = document.createElement('label');
    serialLabel.htmlFor = \`serial\${deviceIndex}\`;
    serialLabel.textContent = 'Serial Number:';
    serialLabel.style.display = 'block';
    serialLabel.style.marginTop = '10px';
    serialLabel.style.fontWeight = 'bold';
    
    const serialInput = document.createElement('input');
    serialInput.type = 'text';
    serialInput.id = \`serial\${deviceIndex}\`;
    serialInput.name = \`serial\${deviceIndex}\`;
    serialInput.style.width = '95%';
    serialInput.style.padding = '5px';
    serialInput.style.fontSize = '18px';
    serialInput.style.backgroundColor = '#eee';
    serialInput.style.marginTop = '5px';
    
    // Description textarea
    const descLabel = document.createElement('label');
    descLabel.htmlFor = \`description\${deviceIndex}\`;
    descLabel.textContent = 'Issue Description:';
    descLabel.style.display = 'block';
    descLabel.style.marginTop = '10px';
    descLabel.style.fontWeight = 'bold';
    
    const descTextarea = document.createElement('textarea');
    descTextarea.id = \`description\${deviceIndex}\`;
    descTextarea.name = \`description\${deviceIndex}\`;
    descTextarea.rows = 3;
    descTextarea.style.width = '95%';
    descTextarea.style.padding = '5px';
    descTextarea.style.fontSize = '18px';
    descTextarea.style.backgroundColor = '#eee';
    descTextarea.style.marginTop = '5px';
    
    deviceDetailsSection.appendChild(serialLabel);
    deviceDetailsSection.appendChild(serialInput);
    deviceDetailsSection.appendChild(descLabel);
    deviceDetailsSection.appendChild(descTextarea);
    
    deviceEntry.appendChild(deviceDetailsSection);
    
    // Add the complete device entry to the container
    document.getElementById('devices-container').appendChild(deviceEntry);
    
    // Set up event listeners for this device entry
    setupDeviceEventListeners(deviceIndex);
    
    // If a current return address is active, apply it to this device
    if (currentReturnAddressIndex !== null) {
      const toggleBtn = deviceEntry.querySelector('button');
      toggleBtn.textContent = \`Using Return Address from Device #\${currentReturnAddressIndex}\`;
      toggleBtn.style.backgroundColor = '#e6f7ff';
      toggleBtn.disabled = true;
    }
    
    return deviceEntry;
  }
  
  // Set up event listeners for a device entry
  function setupDeviceEventListeners(deviceIndex) {
    // Toggle button for alternate address
    const toggleButton = document.querySelector(\`.device-entry[data-index="\${deviceIndex}"] button\`);
    toggleButton.addEventListener('click', function() {
      const addressSection = document.getElementById(\`alternate-address-\${deviceIndex}\`);
      if (addressSection.style.display === 'none') {
        addressSection.style.display = 'block';
        toggleButton.textContent = 'Hide Return Address';
        toggleButton.style.backgroundColor = '#e6f7ff';
      } else {
        addressSection.style.display = 'none';
        toggleButton.textContent = 'Specify Different Return Address';
        toggleButton.style.backgroundColor = '#eee';
      }
    });
    
    // Checkbox for using address for subsequent devices
    const useForAllCheckbox = document.getElementById(\`use_for_all_\${deviceIndex}\`);
    if (useForAllCheckbox) {
      useForAllCheckbox.addEventListener('change', function() {
        if (this.checked) {
          // Set this device as the current return address for new devices
          currentReturnAddressIndex = deviceIndex;
          
          // Disable address toggle buttons for all subsequent devices
          document.querySelectorAll('.device-entry').forEach(entry => {
            const entryIndex = parseInt(entry.dataset.index, 10);
            if (entryIndex > deviceIndex) {
              const btn = entry.querySelector('button');
              if (btn) {
                btn.textContent = \`Using Return Address from Device #\${deviceIndex}\`;
                btn.style.backgroundColor = '#e6f7ff';
                btn.disabled = true;
              }
            }
          });
        } else {
          // If unchecked, remove this as the current return address
          if (currentReturnAddressIndex === deviceIndex) {
            currentReturnAddressIndex = null;
            
            // Re-enable address toggle buttons for all subsequent devices
            document.querySelectorAll('.device-entry').forEach(entry => {
              const entryIndex = parseInt(entry.dataset.index, 10);
              if (entryIndex > deviceIndex) {
                const btn = entry.querySelector('button');
                if (btn) {
                  btn.textContent = 'Specify Different Return Address';
                  btn.style.backgroundColor = '#eee';
                  btn.disabled = false;
                }
              }
            });
          }
        }
      });
    }
    
    // When description field is focused, add new device entry if this is the last one
    const descTextarea = document.getElementById(\`description\${deviceIndex}\`);
    const serialInput = document.getElementById(\`serial\${deviceIndex}\`);
    
    descTextarea.addEventListener('focus', function() {
      if (deviceIndex === deviceCount && serialInput.value.trim() !== '') {
        addDeviceEntry();
      }
    });
    
    // Also check after typing in the serial field
    serialInput.addEventListener('input', function() {
      if (deviceIndex === deviceCount && this.value.trim() !== '') {
        // Check if the description already has focus
        if (document.activeElement !== descTextarea) {
          // If not, we won't add a new entry yet - wait for description focus
        }
      }
    });
  }
  
  // Before form submission, prepare the data to include alternate addresses
  document.getElementById('rmaForm').addEventListener('submit', function(e) {
    // This is handled by myFetch function, no need to preventDefault()
    
    // Add metadata about return addresses as hidden fields
    document.querySelectorAll('.device-entry').forEach(entry => {
      const index = entry.dataset.index;
      const alternateAddressSection = document.getElementById(\`alternate-address-\${index}\`);
      
      // If this device has an alternate address specified and it's visible
      if (alternateAddressSection && alternateAddressSection.style.display !== 'none') {
        // Create a hidden field to indicate this device has an alternate address
        const hasAltAddressField = document.createElement('input');
        hasAltAddressField.type = 'hidden';
        hasAltAddressField.id = \`has_alt_address_\${index}\`;
        hasAltAddressField.name = \`has_alt_address_\${index}\`;
        hasAltAddressField.value = 'true';
        entry.appendChild(hasAltAddressField);
        
        // Create a hidden field for which address to use for this device
        const addressSourceField = document.createElement('input');
        addressSourceField.type = 'hidden';
        addressSourceField.id = \`address_source_\${index}\`;
        addressSourceField.name = \`address_source_\${index}\`;
        addressSourceField.value = index; // Use its own address
        entry.appendChild(addressSourceField);
      } else if (currentReturnAddressIndex !== null && parseInt(index, 10) > currentReturnAddressIndex) {
        // This device should use another device's address
        const addressSourceField = document.createElement('input');
        addressSourceField.type = 'hidden';
        addressSourceField.id = \`address_source_\${index}\`;
        addressSourceField.name = \`address_source_\${index}\`;
        addressSourceField.value = currentReturnAddressIndex;
        entry.appendChild(addressSourceField);
      }
    });
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