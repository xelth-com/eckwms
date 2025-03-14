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

// Serve RMA form page with React
// routes/rma.js
router.get('/', (req, res) => {

});

// Generate RMA form page
router.all('/generate', (req, res) => {
  const timestamp = Math.floor(Date.now() / 1000);
  const rmaCode = `RMA${timestamp}${betrugerCrc(timestamp)}`;
  
  res.send(`
    <span class="text3">
      <div id="rma-form-container" data-rma-code="${rmaCode}">
        <div class="cellPaper" style="text-align: center; padding: 40px;">
          <span class="textM3">Загрузка формы RMA...</span>
        </div>
      </div>
    </span>
  `);
});

// Submit RMA form
router.post('/submit', async (req, res) => {
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
    
    // Send PDF
    res.writeHead(200, {
      'Content-Type': 'application/pdf',
      'Content-Disposition': 'attachment; filename="rma.pdf"',
      'Content-Length': pdfBuffer.length
    });
    
    res.end(pdfBuffer);
    
    // Create order after sending the PDF
    await createOrderFromRma(formattedInput, rmaJson);
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

// Helper function to create order from RMA data
async function createOrderFromRma(formattedInput, rmaJson) {
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
  
  global.orders.set(formattedInput, tempObj);
  
  try {
    await writeLargeMapToFile(global.orders, resolve(`${global.baseDirectory}base/orders.json`));
  } catch (err) {
    console.error(err);
  }
}

// Check RMA status
router.get('/status/:rmaId', (req, res) => {
  const rmaId = req.params.rmaId;
  const betCode = 'o000' + rmaId;
  
  // ...существующий код для проверки статуса RMA...
});

module.exports = router;