// controllers/orderController.js
const logger = require('../utils/logging');
const { ApiError, createNotFoundError, createBadRequestError, createForbiddenError } = require('../middleware/errorHandler');
const pdfService = require('../services/pdfService');

/**
 * Get all orders with pagination and filtering
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function getAllOrders(req, res, next) {
  try {
    // Extract query parameters
    const page = parseInt(req.query.page) || 1;
    const limit = parseInt(req.query.limit) || 20;
    const search = req.query.search || '';
    const status = req.query.status || '';
    const startDate = req.query.startDate ? new Date(req.query.startDate) : null;
    const endDate = req.query.endDate ? new Date(req.query.endDate) : null;
    
    // Validate parameters
    if (page < 1 || limit < 1 || limit > 100) {
      return next(createBadRequestError('Invalid pagination parameters'));
    }
    
    // Get orders from storage service
    const ordersCollection = global.storageService.getCollection('orders');
    if (!ordersCollection) {
      return next(createNotFoundError('Orders collection not found'));
    }
    
    // Filter orders based on query parameters
    let filteredOrders = Array.from(ordersCollection.values());
    
    // Apply search filter (case-insensitive)
    if (search) {
      const searchLower = search.toLowerCase();
      filteredOrders = filteredOrders.filter(order => {
        // Search in order number
        if (order.sn && order.sn[0] && order.sn[0].toLowerCase().includes(searchLower)) {
          return true;
        }
        
        // Search in company name
        if (order.comp && order.comp.toLowerCase().includes(searchLower)) {
          return true;
        }
        
        // Search in contact person
        if (order.pers && order.pers.toLowerCase().includes(searchLower)) {
          return true;
        }
        
        // Search in declarations
        if (order.decl && Array.isArray(order.decl)) {
          for (const decl of order.decl) {
            if (Array.isArray(decl) && decl.length > 1 && 
                (decl[0]?.toLowerCase().includes(searchLower) || 
                 decl[1]?.toLowerCase().includes(searchLower))) {
              return true;
            }
          }
        }
        
        return false;
      });
    }
    
    // Apply status filter
    if (status) {
      filteredOrders = filteredOrders.filter(order => order.st === status);
    }
    
    // Apply date range filter
    if (startDate || endDate) {
      filteredOrders = filteredOrders.filter(order => {
        if (!order.sn || !order.sn[1]) return false;
        
        const orderDate = new Date(order.sn[1] * 1000);
        
        if (startDate && orderDate < startDate) return false;
        if (endDate && orderDate > endDate) return false;
        
        return true;
      });
    }
    
    // Sort orders by timestamp (newest first)
    filteredOrders.sort((a, b) => {
      const aTime = a.sn && a.sn.length > 1 ? a.sn[1] : 0;
      const bTime = b.sn && b.sn.length > 1 ? b.sn[1] : 0;
      return bTime - aTime;
    });
    
    // Apply pagination
    const startIndex = (page - 1) * limit;
    const endIndex = page * limit;
    const paginatedOrders = filteredOrders.slice(startIndex, endIndex);
    
    // Format orders for response
    const formattedOrders = paginatedOrders.map(order => formatOrder(order));
    
    // Return paginated results
    return res.status(200).json({
      orders: formattedOrders,
      pagination: {
        total: filteredOrders.length,
        page,
        limit,
        pages: Math.ceil(filteredOrders.length / limit)
      }
    });
  } catch (error) {
    logger.error(`Error getting orders: ${error.message}`);
    next(error);
  }
}

/**
 * Get order by RMA number
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function getOrderByRmaNumber(req, res, next) {
  try {
    const { rmaNumber } = req.params;
    
    // RMA numbers start with "RMA" but order serial numbers are prefixed with "o"
    const orderSerialNumber = `o${rmaNumber.replace(/^RMA/, '000')}`;
    
    // Get order from storage
    const order = global.storageService.getItem('orders', orderSerialNumber);
    
    if (!order) {
      return next(createNotFoundError(`Order with RMA number ${rmaNumber} not found`));
    }
    
    // Check if user has permission to access this order
    if (req.user.r && req.user.r !== rmaNumber.replace(/^RMA/, '')) {
      const isAdmin = req.user.a === 'a';
      const isElevated = req.user.a === 'p';
      
      if (!isAdmin && !isElevated) {
        return next(createForbiddenError('You do not have permission to access this order'));
      }
    }
    
    // Get order contents
    const orderContents = await getOrderContents(order);
    
    // Format order for response
    const formattedOrder = formatOrder(order);
    
    // Return order with contents
    return res.status(200).json({
      order: formattedOrder,
      contents: orderContents
    });
  } catch (error) {
    logger.error(`Error getting order by RMA number: ${error.message}`);
    next(error);
  }
}

/**
 * Create a new RMA order
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function createOrder(req, res, next) {
  try {
    // Generate a new RMA number
    const timestamp = Math.floor(Date.now() / 1000);
    const rmaNumber = `RMA${timestamp}`;
    const serialNumber = `o000${timestamp}`;
    
    // Extract form data
    const {
      company,
      person,
      street,
      postal,
      country,
      email,
      invoice_email,
      phone,
      resellerName
    } = req.body;
    
    // Validate required fields
    if (!company || !street || !postal || !country || !email) {
      return next(createBadRequestError('Missing required fields'));
    }
    
    // Process item declarations
    const declarations = [];
    for (let i = 1; i <= 5; i++) {
      const serialKey = `serial${i}`;
      const descKey = `description${i}`;
      
      if (req.body[serialKey] && req.body[descKey]) {
        declarations.push([
          req.body[serialKey],
          req.body[descKey]
        ]);
      }
    }
    
    // Split address fields
    const addressInfo = splitStreetAndHouseNumber(street);
    const postalInfo = splitPostalCodeAndCity(postal);
    
    // Create order model
    const Order = require('../models/order');
    const newOrder = new Order(serialNumber);
    
    // Set order properties
    newOrder.comp = company;
    newOrder.pers = person || '';
    newOrder.str = addressInfo.street;
    newOrder.hs = addressInfo.houseNumber;
    newOrder.zip = postalInfo.postalCode;
    newOrder.cit = postalInfo.city;
    newOrder.ctry = country;
    newOrder.cem = email;
    newOrder.iem = invoice_email || '';
    newOrder.ph = phone || '';
    newOrder.decl = declarations;
    newOrder.st = 'pending'; // Initial status
    
    // Add reseller information if provided
    if (resellerName) {
      newOrder.reseller = resellerName;
    }
    
    // Save to storage
    const saved = global.storageService.saveItem('orders', serialNumber, newOrder);
    
    if (!saved) {
      return next(createBadRequestError('Failed to create order'));
    }
    
    // Generate RMA tokens
    const limitedToken = global.authService.createRmaToken(rmaNumber.replace(/^RMA/, ''), 'l');
    const fullToken = global.authService.createRmaToken(rmaNumber.replace(/^RMA/, ''), 'p');
    
    // Generate RMA PDF
    let pdfBuffer;
    try {
      pdfBuffer = await pdfService.generateRmaPdf({
        ...req.body,
        rma: rmaNumber,
        declarations,
        limitedToken,
        fullToken
      });
    } catch (error) {
      logger.error(`Error generating RMA PDF: ${error.message}`);
      // Continue even if PDF generation fails
    }
    
    // Record action in history
    await global.historyService.recordHistory('orders', serialNumber, 'create', {
      user: req.user?.u || 'anonymous',
      timestamp: Math.floor(Date.now() / 1000),
      data: {
        company,
        email,
        declarations: declarations.length
      }
    });
    
    // Format order for response
    const formattedOrder = formatOrder(newOrder);
    
    // Return PDF if generated, otherwise return JSON
    if (pdfBuffer) {
      res.setHeader('Content-Type', 'application/pdf');
      res.setHeader('Content-Disposition', `attachment; filename="${rmaNumber}.pdf"`);
      res.setHeader('Content-Length', pdfBuffer.length);
      res.send(pdfBuffer);
    } else {
      return res.status(201).json({
        message: 'Order created successfully',
        order: formattedOrder,
        tokens: {
          limited: limitedToken,
          full: fullToken
        }
      });
    }
  } catch (error) {
    logger.error(`Error creating order: ${error.message}`);
    next(error);
  }
}

/**
 * Update an RMA order
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function updateOrder(req, res, next) {
  try {
    const { rmaNumber } = req.params;
    const orderSerialNumber = `o${rmaNumber.replace(/^RMA/, '000')}`;
    
    // Get order from storage
    const order = global.storageService.getItem('orders', orderSerialNumber);
    
    if (!order) {
      return next(createNotFoundError(`Order with RMA number ${rmaNumber} not found`));
    }
    
    // Extract fields to update
    const {
      company,
      person,
      street,
      postal,
      country,
      email,
      invoice_email,
      phone,
      status,
      notes
    } = req.body;
    
    // Update fields if provided
    if (company) order.comp = company;
    if (person) order.pers = person;
    
    if (street) {
      const addressInfo = splitStreetAndHouseNumber(street);
      order.str = addressInfo.street;
      order.hs = addressInfo.houseNumber;
    }
    
    if (postal) {
      const postalInfo = splitPostalCodeAndCity(postal);
      order.zip = postalInfo.postalCode;
      order.cit = postalInfo.city;
    }
    
    if (country) order.ctry = country;
    if (email) order.cem = email;
    if (invoice_email) order.iem = invoice_email;
    if (phone) order.ph = phone;
    
    // Update status
    if (status && ['pending', 'processing', 'shipping', 'completed', 'cancelled'].includes(status)) {
      order.st = status;
    }
    
    // Add notes
    if (notes) {
      if (!order.notes) {
        order.notes = [];
      }
      
      order.notes.push({
        text: notes,
        user: req.user.u,
        timestamp: Math.floor(Date.now() / 1000)
      });
    }
    
    // Save to storage
    const saved = global.storageService.saveItem('orders', orderSerialNumber, order);
    
    if (!saved) {
      return next(createBadRequestError('Failed to update order'));
    }
    
    // Record action in history
    await global.historyService.recordHistory('orders', orderSerialNumber, 'update', {
      user: req.user.u,
      timestamp: Math.floor(Date.now() / 1000),
      changes: req.body
    });
    
    // Format order for response
    const formattedOrder = formatOrder(order);
    
    // Return updated order
    return res.status(200).json({
      message: 'Order updated successfully',
      order: formattedOrder
    });
  } catch (error) {
    logger.error(`Error updating order: ${error.message}`);
    next(error);
  }
}

/**
 * Add items to an RMA order
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function addItemsToOrder(req, res, next) {
  try {
    const { rmaNumber } = req.params;
    const { items } = req.body;
    
    const orderSerialNumber = `o${rmaNumber.replace(/^RMA/, '000')}`;
    
    // Get order from storage
    const order = global.storageService.getItem('orders', orderSerialNumber);
    
    if (!order) {
      return next(createNotFoundError(`Order with RMA number ${rmaNumber} not found`));
    }
    
    // Validate items
    if (!items || !Array.isArray(items) || items.length === 0) {
      return next(createBadRequestError('Items array is required'));
    }
    
    // Initialize contents array if it doesn't exist
    if (!order.cont || !Array.isArray(order.cont)) {
      order.cont = [];
    }
    
    // Add items to order
    const timestamp = Math.floor(Date.now() / 1000);
    const addedItems = [];
    
    for (const itemId of items) {
      // Check if item exists
      const item = global.storageService.getItem('items', itemId);
      if (!item) {
        logger.warn(`Item ${itemId} not found, skipping`);
        continue;
      }
      
      // Check if item is already in the order
      const existingItemIndex = order.cont.findIndex(entry => 
        Array.isArray(entry) && entry.length > 0 && entry[0] === itemId
      );
      
      if (existingItemIndex >= 0) {
        // Update timestamp
        order.cont[existingItemIndex][1] = timestamp;
      } else {
        // Add new item
        order.cont.push([itemId, timestamp]);
        addedItems.push(itemId);
        
        // Update item to record association with this order
        if (!item.ord || !Array.isArray(item.ord)) {
          item.ord = [];
        }
        
        item.ord.push([orderSerialNumber, timestamp]);
        global.storageService.saveItem('items', itemId, item);
      }
    }
    
    // Save order to storage
    const saved = global.storageService.saveItem('orders', orderSerialNumber, order);
    
    if (!saved) {
      return next(createBadRequestError('Failed to add items to order'));
    }
    
    // Record action in history
    await global.historyService.recordHistory('orders', orderSerialNumber, 'add_items', {
      user: req.user.u,
      timestamp,
      items: addedItems
    });
    
    // Return updated order
    return res.status(200).json({
      message: `Added ${addedItems.length} items to order`,
      order: formatOrder(order)
    });
  } catch (error) {
    logger.error(`Error adding items to order: ${error.message}`);
    next(error);
  }
}

/**
 * Remove item from an RMA order
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function removeItemFromOrder(req, res, next) {
  try {
    const { rmaNumber, itemSerialNumber } = req.params;
    
    const orderSerialNumber = `o${rmaNumber.replace(/^RMA/, '000')}`;
    
    // Get order from storage
    const order = global.storageService.getItem('orders', orderSerialNumber);
    
    if (!order) {
      return next(createNotFoundError(`Order with RMA number ${rmaNumber} not found`));
    }
    
    // Check if order has contents
    if (!order.cont || !Array.isArray(order.cont) || order.cont.length === 0) {
      return next(createNotFoundError(`Item ${itemSerialNumber} not found in order`));
    }
    
    // Find item in order contents
    const itemIndex = order.cont.findIndex(entry => 
      Array.isArray(entry) && entry.length > 0 && entry[0] === itemSerialNumber
    );
    
    if (itemIndex === -1) {
      return next(createNotFoundError(`Item ${itemSerialNumber} not found in order`));
    }
    
    // Remove item from order
    order.cont.splice(itemIndex, 1);
    
    // Save order to storage
    const saved = global.storageService.saveItem('orders', orderSerialNumber, order);
    
    if (!saved) {
      return next(createBadRequestError('Failed to remove item from order'));
    }
    
    // Update item to remove association with this order
    const item = global.storageService.getItem('items', itemSerialNumber);
    if (item && item.ord && Array.isArray(item.ord)) {
      const orderIndex = item.ord.findIndex(entry => 
        Array.isArray(entry) && entry.length > 0 && entry[0] === orderSerialNumber
      );
      
      if (orderIndex !== -1) {
        item.ord.splice(orderIndex, 1);
        global.storageService.saveItem('items', itemSerialNumber, item);
      }
    }
    
    // Record action in history
    await global.historyService.recordHistory('orders', orderSerialNumber, 'remove_item', {
      user: req.user.u,
      timestamp: Math.floor(Date.now() / 1000),
      item: itemSerialNumber
    });
    
    // Return updated order
    return res.status(200).json({
      message: 'Item removed from order successfully',
      order: formatOrder(order)
    });
  } catch (error) {
    logger.error(`Error removing item from order: ${error.message}`);
    next(error);
  }
}

/**
 * Add box to an RMA order
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function addBoxToOrder(req, res, next) {
  try {
    const { rmaNumber } = req.params;
    const { boxId } = req.body;
    
    if (!boxId) {
      return next(createBadRequestError('Box ID is required'));
    }
    
    const orderSerialNumber = `o${rmaNumber.replace(/^RMA/, '000')}`;
    
    // Get order from storage
    const order = global.storageService.getItem('orders', orderSerialNumber);
    
    if (!order) {
      return next(createNotFoundError(`Order with RMA number ${rmaNumber} not found`));
    }
    
    // Check if box exists
    const box = global.storageService.getItem('boxes', boxId);
    
    if (!box) {
      return next(createNotFoundError(`Box with ID ${boxId} not found`));
    }
    
    // Initialize contents array if it doesn't exist
    if (!order.cont || !Array.isArray(order.cont)) {
      order.cont = [];
    }
    
    // Check if box is already in the order
    const timestamp = Math.floor(Date.now() / 1000);
    const existingBoxIndex = order.cont.findIndex(entry => 
      Array.isArray(entry) && entry.length > 0 && entry[0] === boxId
    );
    
    if (existingBoxIndex >= 0) {
      // Update timestamp
      order.cont[existingBoxIndex][1] = timestamp;
    } else {
      // Add new box
      order.cont.push([boxId, timestamp]);
      
      // Update box to record association with this order
      if (!box.ord || !Array.isArray(box.ord)) {
        box.ord = [];
      }
      
      box.ord.push([orderSerialNumber, timestamp]);
      global.storageService.saveItem('boxes', boxId, box);
    }
    
    // Save order to storage
    const saved = global.storageService.saveItem('orders', orderSerialNumber, order);
    
    if (!saved) {
      return next(createBadRequestError('Failed to add box to order'));
    }
    
    // Record action in history
    await global.historyService.recordHistory('orders', orderSerialNumber, 'add_box', {
      user: req.user?.u || 'anonymous',
      timestamp,
      box: boxId
    });
    
    // Return updated order
    return res.status(200).json({
      message: 'Box added to order successfully',
      order: formatOrder(order)
    });
  } catch (error) {
    logger.error(`Error adding box to order: ${error.message}`);
    next(error);
  }
}

/**
 * Export order data as PDF
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function exportOrderPdf(req, res, next) {
  try {
    const { rmaNumber } = req.params;
    
    const orderSerialNumber = `o${rmaNumber.replace(/^RMA/, '000')}`;
    
    // Get order from storage
    const order = global.storageService.getItem('orders', orderSerialNumber);
    
    if (!order) {
      return next(createNotFoundError(`Order with RMA number ${rmaNumber} not found`));
    }
    
    // Check if user has permission to access this order
    if (req.user.r && req.user.r !== rmaNumber.replace(/^RMA/, '')) {
      const isAdmin = req.user.a === 'a';
      const isElevated = req.user.a === 'p';
      
      if (!isAdmin && !isElevated) {
        return next(createForbiddenError('You do not have permission to access this order'));
      }
    }
    
    // Get order contents
    const orderContents = await getOrderContents(order);
    
    // Format order data for PDF
    const orderData = {
      ...formatOrder(order),
      contents: orderContents,
      rma: `RMA${orderSerialNumber.replace(/^o000/, '')}`
    };
    
    // Generate PDF
    try {
      const pdfBuffer = await pdfService.generateOrderPdf(orderData);
      
      // Return PDF file
      res.setHeader('Content-Type', 'application/pdf');
      res.setHeader('Content-Disposition', `attachment; filename="RMA${orderSerialNumber.replace(/^o000/, '')}.pdf"`);
      res.setHeader('Content-Length', pdfBuffer.length);
      res.send(pdfBuffer);
    } catch (error) {
      logger.error(`Error generating order PDF: ${error.message}`);
      return next(createBadRequestError('Failed to generate PDF'));
    }
  } catch (error) {
    logger.error(`Error exporting order PDF: ${error.message}`);
    next(error);
  }
}

/**
 * Get order status
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function getOrderStatus(req, res, next) {
  try {
    const { rmaNumber } = req.params;
    
    const orderSerialNumber = `o${rmaNumber.replace(/^RMA/, '000')}`;
    
    // Get order from storage
    const order = global.storageService.getItem('orders', orderSerialNumber);
    
    if (!order) {
      return next(createNotFoundError(`Order with RMA number ${rmaNumber} not found`));
    }
    
    // Calculate package count
    const packageCount = order.cont && Array.isArray(order.cont) ? order.cont.length : 0;
    
    // Check if order is completed (packages reached shipping location)
    let completed = false;
    let completedAt = null;
    
    if (order.st === 'completed') {
      completed = true;
      
      // Find completion timestamp from order history
      const history = await global.historyService.getHistory('orders', orderSerialNumber, {
        action: 'status_change',
        limit: 100
      });
      
      const completionEvent = history.find(event => 
        event.data && event.data.newStatus === 'completed'
      );
      
      if (completionEvent) {
        completedAt = completionEvent.timestamp;
      }
    }
    
    // Check for diagnosis and repair timestamps
    let diagnosisAt = null;
    let repairAt = null;
    let receivedAt = null;
    
    // Find diagnosis timestamp from order history
    const diagnosisEvent = await global.historyService.getHistory('orders', orderSerialNumber, {
      action: 'diagnosis',
      limit: 1
    });
    
    if (diagnosisEvent.length > 0) {
      diagnosisAt = diagnosisEvent[0].timestamp;
    }
    
    // Find repair timestamp from order history
    const repairEvent = await global.historyService.getHistory('orders', orderSerialNumber, {
      action: 'repair',
      limit: 1
    });
    
    if (repairEvent.length > 0) {
      repairAt = repairEvent[0].timestamp;
    }
    
    // Find reception timestamp from order history
    const receptionEvent = await global.historyService.getHistory('orders', orderSerialNumber, {
      action: 'received',
      limit: 1
    });
    
    if (receptionEvent.length > 0) {
      receivedAt = receptionEvent[0].timestamp;
    }
    
    // Generate a limited access token for frontend
    const limitedToken = global.authService.createRmaToken(rmaNumber.replace(/^RMA/, ''), 'l');
    
    // Format status response
    const status = {
      created_at: order.sn && order.sn.length > 1 ? new Date(order.sn[1] * 1000) : null,
      package_count: packageCount,
      status: order.st || 'pending',
      completed,
      completed_at: completedAt,
      diagnosis_at: diagnosisAt,
      repair_at: repairAt,
      received_at: receivedAt
    };
    
    // Return status
    return res.status(200).json({
      status,
      token: limitedToken
    });
  } catch (error) {
    logger.error(`Error getting order status: ${error.message}`);
    next(error);
  }
}

/**
 * Get contents of an order (items and boxes)
 * @param {Object} order - Order object
 * @returns {Promise<Object>} Order contents
 */
async function getOrderContents(order) {
  const contents = {
    boxes: [],
    items: []
  };
  
  if (!order.cont || !Array.isArray(order.cont) || order.cont.length === 0) {
    return contents;
  }
  
  // Process each content entry
  for (const entry of order.cont) {
    if (!Array.isArray(entry) || entry.length < 2) continue;
    
    const [id, timestamp] = entry;
    
    // Check if it's a box
    if (id.startsWith('b')) {
      const box = global.storageService.getItem('boxes', id);
      if (box) {
        // Get box contents
        const boxItems = [];
        
        if (box.cont && Array.isArray(box.cont)) {
          for (const itemEntry of box.cont) {
            if (!Array.isArray(itemEntry) || itemEntry.length < 1) continue;
            
            const itemId = itemEntry[0];
            const item = global.storageService.getItem('items', itemId);
            
            if (item) {
              boxItems.push(formatItem(item));
            }
          }
        }
        
        contents.boxes.push({
          ...formatBox(box),
          items: boxItems
        });
      }
    } 
    // Check if it's an item (direct association)
    else if (id.startsWith('i')) {
      const item = global.storageService.getItem('items', id);
      if (item) {
        contents.items.push(formatItem(item));
      }
    }
  }
  
  return contents;
}

/**
 * Format order for API response
 * @param {Object} order - Raw order object
 * @returns {Object} Formatted order
 */
function formatOrder(order) {
  // Create a deep copy of the order
  const formatted = JSON.parse(JSON.stringify(order));
  
  // Format serial number and RMA number
  if (formatted.sn && Array.isArray(formatted.sn) && formatted.sn.length > 0) {
    formatted.serial_number = formatted.sn[0];
    formatted.rma_number = `RMA${formatted.sn[0].replace(/^o000/, '')}`;
    
    if (formatted.sn.length > 1) {
      formatted.created_at = new Date(formatted.sn[1] * 1000);
    }
    
    delete formatted.sn;
  }
  
  // Format company and contact information
  if (formatted.comp) {
    formatted.company = formatted.comp;
    delete formatted.comp;
  }
  
  if (formatted.pers) {
    formatted.person = formatted.pers;
    delete formatted.pers;
  }
  
  // Format address
  if (formatted.str || formatted.hs) {
    formatted.street = (formatted.str || '') + ' ' + (formatted.hs || '');
    delete formatted.str;
    delete formatted.hs;
  }
  
  if (formatted.zip || formatted.cit) {
    formatted.postal = (formatted.zip || '') + ' ' + (formatted.cit || '');
    delete formatted.zip;
    delete formatted.cit;
  }
  
  if (formatted.ctry) {
    formatted.country = formatted.ctry;
    delete formatted.ctry;
  }
  
  // Format contact information
  if (formatted.cem) {
    formatted.contact_email = formatted.cem;
    delete formatted.cem;
  }
  
  if (formatted.iem) {
    formatted.invoice_email = formatted.iem;
    delete formatted.iem;
  }
  
  if (formatted.ph) {
    formatted.phone = formatted.ph;
    delete formatted.ph;
  }
  
  // Format status
  if (formatted.st) {
    formatted.status = formatted.st;
    delete formatted.st;
  }
  
  // Format declarations
  if (formatted.decl && Array.isArray(formatted.decl)) {
    formatted.declarations = formatted.decl.map(decl => {
      if (Array.isArray(decl) && decl.length >= 2) {
        return {
          serial_number: decl[0],
          description: decl[1]
        };
      }
      return null;
    }).filter(item => item !== null);
    
    delete formatted.decl;
  }
  
  // Format contents
  if (formatted.cont && Array.isArray(formatted.cont)) {
    formatted.contents = formatted.cont.map(cont => {
      if (Array.isArray(cont) && cont.length >= 2) {
        return {
          id: cont[0],
          timestamp: cont[1]
        };
      }
      return null;
    }).filter(item => item !== null);
    
    delete formatted.cont;
  }
  
  return formatted;
}

/**
 * Format box for API response (simplified)
 * @param {Object} box - Raw box object
 * @returns {Object} Formatted box
 */
function formatBox(box) {
  const formatted = {};
  
  // Format basic information
  if (box.sn && Array.isArray(box.sn) && box.sn.length > 0) {
    formatted.serial_number = box.sn[0];
    
    if (box.sn.length > 1) {
      formatted.created_at = new Date(box.sn[1] * 1000);
    }
  }
  
  // Format description
  if (box.desc && Array.isArray(box.desc) && box.desc.length > 0) {
    formatted.description = box.desc[0];
  }
  
  // Format barcodes
  if (box.brc && Array.isArray(box.brc) && box.brc.length > 0) {
    formatted.barcodes = box.brc;
  }
  
  // Format class
  if (box.cl) {
    formatted.class_id = box.cl;
  }
  
  // Format location
  if (box.loc && Array.isArray(box.loc) && box.loc.length > 0) {
    const lastLocation = box.loc[box.loc.length - 1];
    if (Array.isArray(lastLocation) && lastLocation.length > 0) {
      formatted.current_location_id = lastLocation[0];
    }
  }
  
  return formatted;
}

/**
 * Format item for API response (simplified)
 * @param {Object} item - Raw item object
 * @returns {Object} Formatted item
 */
function formatItem(item) {
  const formatted = {};
  
  // Format basic information
  if (item.sn && Array.isArray(item.sn) && item.sn.length > 0) {
    formatted.serial_number = item.sn[0];
    
    if (item.sn.length > 1) {
      formatted.created_at = new Date(item.sn[1] * 1000);
    }
  }
  
  // Format description
  if (item.desc && Array.isArray(item.desc) && item.desc.length > 0) {
    formatted.description = item.desc[0];
  }
  
  // Format class
  if (item.cl) {
    formatted.class_id = item.cl;
  }
  
  // Format condition
  if (item.cond && Array.isArray(item.cond) && item.cond.length > 0) {
    formatted.condition = item.cond[0];
  }
  
  // Format location
  if (item.loc && Array.isArray(item.loc) && item.loc.length > 0) {
    const lastLocation = item.loc[item.loc.length - 1];
    if (Array.isArray(lastLocation) && lastLocation.length > 0) {
      formatted.current_location_id = lastLocation[0];
    }
  }
  
  return formatted;
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
  getAllOrders,
  getOrderByRmaNumber,
  createOrder,
  updateOrder,
  addItemsToOrder,
  removeItemFromOrder,
  addBoxToOrder,
  exportOrderPdf,
  getOrderStatus
};