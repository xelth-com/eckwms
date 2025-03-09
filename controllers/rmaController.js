// controllers/rmaController.js
const dbService = require('../services/dbService');
const authService = require('../services/authService');
const pdfService = require('../services/pdfService');
const logger = require('../utils/logging');
const { sanitizeInput } = require('../utils/validators');

/**
 * Create a new RMA request
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 */
async function createRmaRequest(req, res) {
  try {
    // Generate timestamp for RMA number
    const timestamp = Math.floor(Date.now() / 1000);
    const rmaNumber = `RMA${timestamp}`;
    
    // Validate and sanitize input fields
    const formData = {};
    const requiredFields = ['company', 'street', 'postal', 'country', 'email'];
    
    // Check required fields
    for (const field of requiredFields) {
      if (!req.body[field]) {
        return res.status(400).json({ 
          message: `Missing required field: ${field}` 
        });
      }
      formData[field] = sanitizeInput(req.body[field]);
    }
    
    // Process optional fields
    const optionalFields = ['person', 'invoice_email', 'phone', 'resellerName'];
    for (const field of optionalFields) {
      if (req.body[field]) {
        formData[field] = sanitizeInput(req.body[field]);
      }
    }
    
    // Process serialNumbers and descriptions
    const declarations = [];
    for (let i = 1; i <= 5; i++) {
      const serialKey = `serial${i}`;
      const descKey = `description${i}`;
      
      if (req.body[serialKey] && req.body[descKey]) {
        declarations.push([
          sanitizeInput(req.body[serialKey]), 
          sanitizeInput(req.body[descKey])
        ]);
      }
    }
    
    // Split address fields
    const addressInfo = splitStreetAndHouseNumber(formData.street);
    const postalInfo = splitPostalCodeAndCity(formData.postal);
    
    // Create RMA order in database
    const client = await dbService.beginTransaction();
    
    try {
      // Insert order record
      const orderResult = await client.query(
        `INSERT INTO orders (
          serial_number, created_at, company, person, street, house_number, 
          postal_code, city, country, contact_email, invoice_email, phone, declarations
        ) VALUES ($1, to_timestamp($2), $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
        RETURNING id, serial_number`,
        [
          `o000${rmaNumber}`,
          timestamp,
          formData.company,
          formData.person || null,
          addressInfo.street,
          addressInfo.houseNumber,
          postalInfo.postalCode,
          postalInfo.city,
          formData.country,
          formData.email,
          formData.invoice_email || null,
          formData.phone || null,
          JSON.stringify(declarations)
        ]
      );
      
      const orderId = orderResult.rows[0].id;
      const serialNumber = orderResult.rows[0].serial_number;
      
      // Create limited and full access tokens
      const limitedToken = authService.createRmaToken(rmaNumber, 'l');
      const fullToken = authService.createRmaToken(rmaNumber, 'p');
      
      // Generate RMA PDF document
      const pdfBuffer = await pdfService.generateRmaPdf({
        ...formData,
        rma: rmaNumber,
        declarations,
        limitedToken,
        fullToken
      });
      
      await dbService.commitTransaction(client);
      
      // Log success
      logger.info(`Created new RMA request: ${rmaNumber}`);
      
      // Return PDF file and tokens
      res.setHeader('Content-Type', 'application/pdf');
      res.setHeader('Content-Disposition', `attachment; filename="${rmaNumber}.pdf"`);
      res.setHeader('Content-Length', pdfBuffer.length);
      res.send(pdfBuffer);
      
    } catch (error) {
      await dbService.rollbackTransaction(client);
      throw error;
    }
  } catch (error) {
    logger.error(`Failed to create RMA request: ${error.message}`);
    res.status(500).json({ message: 'Failed to create RMA request' });
  }
}

/**
 * Get RMA request details
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 */
async function getRmaDetails(req, res) {
  try {
    const { rmaNumber } = req.params;
    
    // Check if user has access to this RMA
    if (req.user.r && req.user.r !== rmaNumber.replace(/^RMA/, '')) {
      return res.status(403).json({ message: 'Access denied to this RMA' });
    }
    
    // Determine access level (full or limited)
    const accessLevel = req.user.a || 'l';
    
    // Fetch order details
    const orderResult = await dbService.query(
      `SELECT o.*, 
        json_agg(DISTINCT b.*) as boxes,
        json_agg(DISTINCT i.*) as items
      FROM orders o
      LEFT JOIN LATERAL (
        SELECT b.serial_number, b.created_at, b.contents, b.location_history, b.current_location_id,
               b.barcodes, b.description
        FROM boxes b
        JOIN jsonb_array_elements(o.contents) AS content ON content->>'id' = b.id
      ) b ON true
      LEFT JOIN LATERAL (
        SELECT i.serial_number, i.created_at, i.actions, i.location_history, i.current_location_id,
               i.class_id, i.attributes, i.condition
        FROM items i
        JOIN boxes b ON b.id = ANY (
          SELECT (content->>'id')::uuid
          FROM jsonb_array_elements(o.contents) AS content
        )
        JOIN jsonb_array_elements(b.contents) AS box_content ON box_content->>'id' = i.id
      ) i ON true
      WHERE o.serial_number = $1
      GROUP BY o.id`,
      [`o000${rmaNumber}`]
    );
    
    if (orderResult.rows.length === 0) {
      return res.status(404).json({ message: 'RMA not found' });
    }
    
    const orderData = orderResult.rows[0];
    
    // Mask sensitive data for limited access
    if (accessLevel === 'l') {
      // Mask fields like email, phone, etc.
      const fieldsToMask = ['contact_email', 'invoice_email', 'phone'];
      
      fieldsToMask.forEach(field => {
        if (orderData[field]) {
          orderData[field] = maskString(orderData[field]);
        }
      });
    }
    
    // Return order details
    res.status(200).json({ order: orderData });
    
  } catch (error) {
    logger.error(`Failed to get RMA details: ${error.message}`);
    res.status(500).json({ message: 'Failed to get RMA details' });
  }
}

/**
 * Check RMA status
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 */
async function checkRmaStatus(req, res) {
  try {
    const { rmaNumber } = req.query;
    
    if (!rmaNumber || !/^RMA\d{10}[A-Za-z0-9]{2}$/.test(rmaNumber)) {
      return res.status(400).json({ message: 'Invalid RMA number format' });
    }
    
    // Get basic status information
    const statusResult = await dbService.query(
      `SELECT 
        serial_number, 
        created_at, 
        (SELECT COUNT(*) FROM jsonb_array_elements(contents)) as package_count,
        EXISTS (
          SELECT 1 FROM boxes b
          JOIN jsonb_array_elements(o.contents) AS content ON content->>'id' = b.id::text
          JOIN jsonb_array_elements(b.location_history) AS loc ON loc->>'id' = 'p000000000000000060'
          WHERE loc->>'id' = 'p000000000000000060'
          AND (loc->>'timestamp')::bigint > extract(epoch from o.created_at)
        ) as completed
      FROM orders o
      WHERE serial_number = $1`,
      [`o000${rmaNumber}`]
    );
    
    if (statusResult.rows.length === 0) {
      return res.status(404).json({ message: 'RMA not found' });
    }
    
    const statusData = statusResult.rows[0];
    
    // Generate a limited access token for frontend
    const limitedToken = authService.createRmaToken(rmaNumber.replace(/^RMA/, ''), 'l');
    
    res.status(200).json({ 
      status: statusData,
      token: limitedToken
    });
    
  } catch (error) {
    logger.error(`Failed to check RMA status: ${error.message}`);
    res.status(500).json({ message: 'Failed to check RMA status' });
  }
}

/**
 * Mask a string for privacy
 * @param {string} str - String to mask
 * @returns {string} Masked string
 */
function maskString(str) {
  return str
    .split(' ')
    .map(word => (word.length > 1 ? word[0] + '*'.repeat(word.length - 1) : word))
    .join(' ');
}

/**
 * Split street and house number
 * @param {string} address - Full address
 * @returns {Object} Object with street and houseNumber
 */
function splitStreetAndHouseNumber(address) {
  try {
    const regex = /^(.*?)(\d{1,5}[A-Za-z\-\/\s]*)$/;
    const match = address.trim().match(regex);
    
    if (match) {
      return {
        street: match[1].trim(),
        houseNumber: match[2].trim()
      };
    } else {
      return { 
        street: address.trim(), 
        houseNumber: '' 
      };
    }
  } catch (error) {
    logger.error(`Error splitting address: ${error.message}`);
    return { 
      street: address.trim(), 
      houseNumber: '' 
    };
  }
}

/**
 * Split postal code and city
 * @param {string} address - Postal code and city
 * @returns {Object} Object with postalCode and city
 */
function splitPostalCodeAndCity(address) {
  try {
    const regex = /(\d{4,6})\s*([A-Za-z]*)/;
    const match = address.trim().match(regex);
    
    if (match) {
      return {
        postalCode: match[1],
        city: match[2] || address.replace(match[1], '').trim()
      };
    } else {
      return { 
        postalCode: '', 
        city: address.trim() 
      };
    }
  } catch (error) {
    logger.error(`Error splitting postal code: ${error.message}`);
    return { 
      postalCode: '', 
      city: address.trim() 
    };
  }
}

module.exports = {
  createRmaRequest,
  getRmaDetails,
  checkRmaStatus
};