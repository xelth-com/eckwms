// routes/rma.js
const express = require('express');
const router = express.Router();
const { generateJWT, betrugerUrlEncrypt, betrugerCrc } = require('../utils/encryption');
const { splitStreetAndHouseNumber, splitPostalCodeAndCity, convertToSerialDescriptionArray } = require('../utils/formatUtils');
const { generatePdfRma } = require('../utils/pdfGenerator');
const { writeLargeMapToFile } = require('../utils/fileUtils');
const { resolve } = require('path');
const fs = require('fs').promises;

// Serve RMA form page
router.get('/', (req, res) => {
    res.sendFile(path.join(__dirname, '../html/rmaForm.html'));
});

// Generate RMA form page
router.get('/generate', (req, res) => {
    const timestamp = Math.floor(Date.now() / 1000);
    const rmaCode = `RMA${timestamp}${betrugerCrc(timestamp)}`;
    
    res.send(`
<div style="font-family: Arial, sans-serif; max-width: 600px; margin: 20px auto;">
  <h2>RMA Form</h2>
  <form id="rmaForm" onsubmit="return myFetch('formSubmit', 'rmaForm', 'pdfRma');">
    <input type="text" id="rma" value="${rmaCode}" readonly required 
           style="font-size: 20px; width: 95%; padding: 5px; background-color: #eee; margin-top: 5px;">
    <!-- Company Information -->
    <label for="company" style="display: block; margin-top: 10px;"><b>Company Name:</b></label>
    <input type="text" id="company" required 
           style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

    <label for="person" style="display: block; margin-top: 10px;">Contact Person:</label>
    <input type="text" id="person" 
           style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

    <label for="street" style="display: block; margin-top: 10px;"><b>Street and House Number:</b></label>
    <input type="text" id="street" required 
           style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

    <label for="postal" style="display: block; margin-top: 10px;"><b>Postal Code / City:</b></label>
    <input type="text" id="postal" required 
           style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

    <label for="country" style="display: block; margin-top: 10px;"><b>Country:</b></label>
    <input type="text" id="country" required 
           style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

    <!-- Contact Information -->
    <label for="email" style="display: block; margin-top: 10px;"><b>Contact Email:</b></label>
    <input type="email" id="email" required 
           style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

    <label for="invoice_email" style="display: block; margin-top: 10px;">E-Invoice Email:</label>
    <input type="email" id="invoice_email" 
           style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

    <label for="phone" style="display: block; margin-top: 10px;">Phone:</label>
    <input type="tel" id="phone" 
           style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

    <label for="resellerName" style="display: block; margin-top: 10px;">In case of warranty, please provide the reseller's name:</label>
    <input type="text" id="resellerName" 
           style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">


        <!-- Serial Numbers and Descriptions -->
        <br><br><br><br>

        <div style="display: flex; flex-wrap: wrap; margin-top: 10px;">
            <div style="flex: 0 0 170px; margin-right: 10px;">
                <label for="serial1" style=" margin-top: 10px;">Serial Number 1:</label><br>
                <input type="text" id="serial1"   style="width: 170px; padding: 5px; font-size: 20px; background-color : #eee; margin-top: 5px;">
            </div>
            <div style="flex: 1 1 300px;">
                <label for="description1" style=" margin-top: 10px;">Issue Description:</label><br>
                <textarea id="description1"  rows="3"  style="width: 95%; min-width: 300px; padding: 5px; font-size: 20px; background-color : #eee; margin-top: 5px;"></textarea>
            </div>
        </div>
        <br>
        <div style="display: flex; flex-wrap: wrap; margin-top: 10px;">
            <div style="flex: 0 0 170px; margin-right: 10px;">
                <label for="serial2" style=" margin-top: 10px;">Serial Number 2:</label><br>
                <input type="text" id="serial2"  style="width: 170px; padding: 5px; font-size: 20px; background-color : #eee; margin-top: 5px;">
            </div>
            <div style="flex: 1 1 300px;">
                <label for="description2" style=" margin-top: 10px;">Issue Description:</label><br>
                <textarea id="description2"  rows="3"  style="width: 95%; min-width: 300px; padding: 5px; font-size: 20px; background-color : #eee; margin-top: 5px;"></textarea>
            </div>
        </div>
        <br>
                <div style="display: flex; flex-wrap: wrap; margin-top: 10px;">
            <div style="flex: 0 0 170px; margin-right: 10px;">
                <label for="serial3" style=" margin-top: 10px;">Serial Number 3:</label><br>
                <input type="text" id="serial3"   style="width: 170px; padding: 5px; font-size: 20px; background-color : #eee; margin-top: 5px;">
            </div>
            <div style="flex: 1 1 300px;">
                <label for="description3" style=" margin-top: 10px;">Issue Description:</label><br>
                <textarea id="description3"  rows="3"  style="width: 95%; min-width: 300px; padding: 5px; font-size: 20px; background-color : #eee; margin-top: 5px;"></textarea>
            </div>
        </div>
        <br>
        <div style="display: flex; flex-wrap: wrap; margin-top: 10px;">
            <div style="flex: 0 0 170px; margin-right: 10px;">
                <label for="serial4" style=" margin-top: 10px;">Serial Number 4:</label><br>
                <input type="text" id="serial4"   style="width: 170px; padding: 5px; font-size: 20px; background-color : #eee; margin-top: 5px;">
            </div>
            <div style="flex: 1 1 300px;">
                <label for="description4" style=" margin-top: 10px;">Issue Description:</label><br>
                <textarea id="description4"  rows="3"  style="width: 95%; min-width: 300px; padding: 5px; font-size: 20px; background-color : #eee; margin-top: 5px;"></textarea>
            </div>
        </div>
        <br>
                <div style="display: flex; flex-wrap: wrap; margin-top: 10px;">
            <div style="flex: 0 0 170px; margin-right: 10px;">
                <label for="serial5" style=" margin-top: 10px;">Serial Number 5:</label><br>
                <input type="text" id="serial5"   style="width: 170px; padding: 5px; font-size: 20px; background-color : #eee; margin-top: 5px;">
            </div>
            <div style="flex: 1 1 300px;">
                <label for="description5" style=" margin-top: 10px;">Issue Description:</label><br>
                <textarea id="description5"  rows="3"  style="width: 95%; min-width: 300px; padding: 5px; font-size: 20px; background-color : #eee; margin-top: 5px;"></textarea>
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
    
    if (!global.orders.has(betCode)) {
        return res.status(404).json({ error: 'RMA not found' });
    }
    
    const order = global.orders.get(betCode);
    
    // Create a sanitized version of the order for public view
    const status = {
        rmaId: rmaId,
        createdAt: order.sn[1],
        serialNumbers: order.decl?.map(([serial, desc]) => ({ serial, description: desc })) || [],
        receivedItems: order.cont?.map(container => {
            const boxId = container[0];
            const registrationTime = container[1];
            
            if (global.boxes.has(boxId)) {
                const box = global.boxes.get(boxId);
                return {
                    boxId: boxId.replace(/^b0+/, ''),
                    registrationTime,
                    contents: box.cont?.map(item => {
                        const itemId = item[0];
                        const itemTime = item[1];
                        return {
                            itemId: itemId.startsWith('i7') ? itemId.slice(-7) : itemId,
                            registrationTime: itemTime
                        };
                    }) || []
                };
            }
            return { boxId: boxId.replace(/^b0+/, ''), registrationTime, contents: [] };
        }) || [],
        completed: order.cont?.some(container => {
            const boxId = container[0];
            if (global.boxes.has(boxId)) {
                const box = global.boxes.get(boxId);
                return box.loc?.some(loc => loc[0] === 'p000000000000000060') || false;
            }
            return false;
        }) || false
    };
    
    res.json(status);
});

module.exports = router;