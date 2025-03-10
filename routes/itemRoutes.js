// controllers/itemController.js
const logger = require('../utils/logging');
const { ApiError, createNotFoundError, createBadRequestError } = require('../middleware/errorHandler');

/**
 * Get all items with pagination and filtering
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function getAllItems(req, res, next) {
  try {
    // Extract query parameters
    const page = parseInt(req.query.page) || 1;
    const limit = parseInt(req.query.limit) || 20;
    const search = req.query.search || '';
    const classId = req.query.classId || '';
    const locationId = req.query.locationId || '';
    
    // Validate parameters
    if (page < 1 || limit < 1 || limit > 100) {
      return next(createBadRequestError('Invalid pagination parameters'));
    }
    
    // Get items from storage service
    const itemsCollection = global.storageService.getCollection('items');
    if (!itemsCollection) {
      return next(createNotFoundError('Items collection not found'));
    }
    
    // Filter items based on query parameters
    let filteredItems = Array.from(itemsCollection.values());
    
    // Apply search filter (case-insensitive)
    if (search) {
      const searchLower = search.toLowerCase();
      filteredItems = filteredItems.filter(item => {
        // Search in serial number
        if (item.sn && item.sn[0] && item.sn[0].toLowerCase().includes(searchLower)) {
          return true;
        }
        
        // Search in barcodes
        if (item.brc && Array.isArray(item.brc) && item.brc.some(bc => bc.toLowerCase().includes(searchLower))) {
          return true;
        }
        
        // Search in description
        if (item.desc && Array.isArray(item.desc) && item.desc.some(d => d && d.toLowerCase().includes(searchLower))) {
          return true;
        }
        
        return false;
      });
    }
    
    // Apply class filter
    if (classId) {
      filteredItems = filteredItems.filter(item => item.cl === classId);
    }
    
    // Apply location filter
    if (locationId) {
      filteredItems = filteredItems.filter(item => {
        return item.loc && 
               Array.isArray(item.loc) && 
               item.loc.length > 0 && 
               item.loc[item.loc.length - 1][0] === locationId;
      });
    }
    
    // Sort items by timestamp (newest first)
    filteredItems.sort((a, b) => {
      const aTime = a.sn && a.sn.length > 1 ? a.sn[1] : 0;
      const bTime = b.sn && b.sn.length > 1 ? b.sn[1] : 0;
      return bTime - aTime;
    });
    
    // Apply pagination
    const startIndex = (page - 1) * limit;
    const endIndex = page * limit;
    const paginatedItems = filteredItems.slice(startIndex, endIndex);
    
    // Format items for response
    const formattedItems = paginatedItems.map(item => formatItem(item));
    
    // Return paginated results
    return res.status(200).json({
      items: formattedItems,
      pagination: {
        total: filteredItems.length,
        page,
        limit,
        pages: Math.ceil(filteredItems.length / limit)
      }
    });
  } catch (error) {
    logger.error(`Error getting items: ${error.message}`);
    next(error);
  }
}

/**
 * Get item by serial number
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function getItemBySerialNumber(req, res, next) {
  try {
    const { serialNumber } = req.params;
    
    // Get item from storage
    const item = global.storageService.getItem('items', serialNumber);
    
    if (!item) {
      return next(createNotFoundError(`Item with serial number ${serialNumber} not found`));
    }
    
    // Format item for response
    const formattedItem = formatItem(item);
    
    // Return item
    return res.status(200).json({
      item: formattedItem
    });
  } catch (error) {
    logger.error(`Error getting item by serial number: ${error.message}`);
    next(error);
  }
}

/**
 * Create a new item
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function createItem(req, res, next) {
  try {
    const { className, description } = req.body;
    
    // Generate a new serial number
    const serialNumber = global.storageService.generateSerialNumber('i');
    
    // Create item model
    const Item = require('../models/item');
    const newItem = new Item(serialNumber, className, description);
    
    // Save to storage
    const saved = global.storageService.saveItem('items', serialNumber, newItem);
    
    if (!saved) {
      return next(createBadRequestError('Failed to create item'));
    }
    
    // Format item for response
    const formattedItem = formatItem(newItem);
    
    // Return created item
    return res.status(201).json({
      message: 'Item created successfully',
      item: formattedItem
    });
  } catch (error) {
    logger.error(`Error creating item: ${error.message}`);
    next(error);
  }
}

/**
 * Update an item
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function updateItem(req, res, next) {
  try {
    const { serialNumber } = req.params;
    const { className, description, condition } = req.body;
    
    // Get item from storage
    const item = global.storageService.getItem('items', serialNumber);
    
    if (!item) {
      return next(createNotFoundError(`Item with serial number ${serialNumber} not found`));
    }
    
    // Update fields if provided
    if (className) {
      item.cl = className;
    }
    
    if (description) {
      // Add new description at the beginning of the array
      if (!item.desc || !Array.isArray(item.desc)) {
        item.desc = [];
      }
      item.desc.unshift(description);
      
      // Limit the number of descriptions to keep
      if (item.desc.length > 5) {
        item.desc = item.desc.slice(0, 5);
      }
    }
    
    if (condition) {
      // Set condition
      if (!item.cond || !Array.isArray(item.cond)) {
        item.cond = [];
      }
      item.cond = [condition]; // Replace current condition
    }
    
    // Save to storage
    const saved = global.storageService.saveItem('items', serialNumber, item);
    
    if (!saved) {
      return next(createBadRequestError('Failed to update item'));
    }
    
    // Format item for response
    const formattedItem = formatItem(item);
    
    // Return updated item
    return res.status(200).json({
      message: 'Item updated successfully',
      item: formattedItem
    });
  } catch (error) {
    logger.error(`Error updating item: ${error.message}`);
    next(error);
  }
}

/**
 * Update item location
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function updateItemLocation(req, res, next) {
  try {
    const { serialNumber } = req.params;
    const { locationId } = req.body;
    
    if (!locationId) {
      return next(createBadRequestError('Location ID is required'));
    }
    
    // Get item from storage
    const item = global.storageService.getItem('items', serialNumber);
    
    if (!item) {
      return next(createNotFoundError(`Item with serial number ${serialNumber} not found`));
    }
    
    // Validate location
    const location = global.storageService.getItem('places', locationId);
    
    if (!location) {
      return next(createNotFoundError(`Location with ID ${locationId} not found`));
    }
    
    // Update item location
    const updated = global.storageService.updateItemLocation(serialNumber, locationId);
    
    if (!updated) {
      return next(createBadRequestError('Failed to update item location'));
    }
    
    // Get updated item
    const updatedItem = global.storageService.getItem('items', serialNumber);
    
    // Format item for response
    const formattedItem = formatItem(updatedItem);
    
    // Return updated item
    return res.status(200).json({
      message: 'Item location updated successfully',
      item: formattedItem
    });
  } catch (error) {
    logger.error(`Error updating item location: ${error.message}`);
    next(error);
  }
}

/**
 * Add barcode to item
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function addBarcode(req, res, next) {
  try {
    const { serialNumber } = req.params;
    const { barcode } = req.body;
    
    if (!barcode) {
      return next(createBadRequestError('Barcode is required'));
    }
    
    // Get item from storage
    const item = global.storageService.getItem('items', serialNumber);
    
    if (!item) {
      return next(createNotFoundError(`Item with serial number ${serialNumber} not found`));
    }
    
    // Initialize barcodes array if it doesn't exist
    if (!item.brc || !Array.isArray(item.brc)) {
      item.brc = [];
    }
    
    // Check if barcode already exists
    if (item.brc.includes(barcode)) {
      return res.status(200).json({
        message: 'Barcode already exists on this item',
        item: formatItem(item)
      });
    }
    
    // Add barcode
    item.brc.push(barcode);
    
    // Save to storage
    const saved = global.storageService.saveItem('items', serialNumber, item);
    
    if (!saved) {
      return next(createBadRequestError('Failed to add barcode to item'));
    }
    
    // Format item for response
    const formattedItem = formatItem(item);
    
    // Return updated item
    return res.status(200).json({
      message: 'Barcode added successfully',
      item: formattedItem
    });
  } catch (error) {
    logger.error(`Error adding barcode to item: ${error.message}`);
    next(error);
  }
}

/**
 * Add action to item
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function addAction(req, res, next) {
  try {
    const { serialNumber } = req.params;
    const { type, message } = req.body;
    
    if (!type || !message) {
      return next(createBadRequestError('Action type and message are required'));
    }
    
    // Get item from storage
    const item = global.storageService.getItem('items', serialNumber);
    
    if (!item) {
      return next(createNotFoundError(`Item with serial number ${serialNumber} not found`));
    }
    
    // Add action
    const timestamp = Math.floor(Date.now() / 1000);
    
    // Initialize actions array if it doesn't exist
    if (!item.actn || !Array.isArray(item.actn)) {
      item.actn = [];
    }
    
    // Add action
    item.actn.push([type, message, timestamp]);
    
    // Limit the number of actions to keep
    if (item.actn.length > 20) {
      item.actn = item.actn.slice(-20);
    }
    
    // Save to storage
    const saved = global.storageService.saveItem('items', serialNumber, item);
    
    if (!saved) {
      return next(createBadRequestError('Failed to add action to item'));
    }
    
    // Format item for response
    const formattedItem = formatItem(item);
    
    // Return updated item
    return res.status(200).json({
      message: 'Action added successfully',
      item: formattedItem
    });
  } catch (error) {
    logger.error(`Error adding action to item: ${error.message}`);
    next(error);
  }
}

/**
 * Get item history
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function getItemHistory(req, res, next) {
  try {
    const { serialNumber } = req.params;
    
    // Get item from storage
    const item = global.storageService.getItem('items', serialNumber);
    
    if (!item) {
      return next(createNotFoundError(`Item with serial number ${serialNumber} not found`));
    }
    
    // Get options
    const limit = parseInt(req.query.limit) || 100;
    const offset = parseInt(req.query.offset) || 0;
    const action = req.query.action || null;
    const startTime = req.query.startTime ? parseInt(req.query.startTime) : 0;
    const endTime = req.query.endTime ? parseInt(req.query.endTime) : Math.floor(Date.now() / 1000);
    
    // Get history
    const history = await global.historyService.getHistory('items', serialNumber, {
      limit,
      offset,
      action,
      startTime,
      endTime
    });
    
    // Return history
    return res.status(200).json({
      history
    });
  } catch (error) {
    logger.error(`Error getting item history: ${error.message}`);
    next(error);
  }
}

/**
 * Find item by barcode
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function findItemByBarcode(req, res, next) {
  try {
    const { barcode } = req.params;
    
    // Get all items
    const itemsCollection = global.storageService.getCollection('items');
    
    if (!itemsCollection) {
      return next(createNotFoundError('Items collection not found'));
    }
    
    // Find item with matching barcode
    const items = Array.from(itemsCollection.values()).filter(item => {
      return item.brc && Array.isArray(item.brc) && item.brc.includes(barcode);
    });
    
    if (items.length === 0) {
      return next(createNotFoundError(`No items found with barcode ${barcode}`));
    }
    
    // Format items for response
    const formattedItems = items.map(item => formatItem(item));
    
    // Return items
    return res.status(200).json({
      items: formattedItems
    });
  } catch (error) {
    logger.error(`Error finding item by barcode: ${error.message}`);
    next(error);
  }
}

/**
 * Format item for API response
 * @param {Object} item - Raw item object
 * @returns {Object} Formatted item
 */
function formatItem(item) {
  // Create a deep copy of the item
  const formatted = JSON.parse(JSON.stringify(item));
  
  // Format creation timestamp
  if (formatted.sn && formatted.sn.length > 1) {
    formatted.created_at = new Date(formatted.sn[1] * 1000);
  }
  
  // Format location history
  if (formatted.loc && Array.isArray(formatted.loc)) {
    formatted.location_history = formatted.loc.map(loc => {
      return {
        id: loc[0],
        timestamp: loc[1]
      };
    });
    
    // Set current location
    if (formatted.location_history.length > 0) {
      formatted.current_location_id = formatted.location_history[formatted.location_history.length - 1].id;
    }
    
    // Remove original loc field
    delete formatted.loc;
  }
  
  // Format actions
  if (formatted.actn && Array.isArray(formatted.actn)) {
    formatted.actions = formatted.actn.map(action => {
      return {
        type: action[0],
        message: action[1],
        timestamp: action[2]
      };
    });
    
    // Remove original actn field
    delete formatted.actn;
  }
  
  // Format barcodes
  if (formatted.brc && Array.isArray(formatted.brc)) {
    formatted.barcodes = formatted.brc;
    delete formatted.brc;
  }
  
  // Format description
  if (formatted.desc && Array.isArray(formatted.desc)) {
    formatted.description = formatted.desc;
    delete formatted.desc;
  }
  
  // Format condition
  if (formatted.cond && Array.isArray(formatted.cond) && formatted.cond.length > 0) {
    formatted.condition = formatted.cond[0];
    delete formatted.cond;
  }
  
  // Rename cl field to class_id
  if (formatted.cl) {
    formatted.class_id = formatted.cl;
    delete formatted.cl;
  }
  
  return formatted;
}

module.exports = {
  getAllItems,
  getItemBySerialNumber,
  createItem,
  updateItem,
  updateItemLocation,
  addBarcode,
  addAction,
  getItemHistory,
  findItemByBarcode
};