// controllers/boxController.js
const logger = require('../utils/logging');
const { ApiError, createNotFoundError, createBadRequestError } = require('../middleware/errorHandler');

/**
 * Get all boxes with pagination and filtering
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function getAllBoxes(req, res, next) {
  try {
    // Extract query parameters
    const page = parseInt(req.query.page) || 1;
    const limit = parseInt(req.query.limit) || 20;
    const search = req.query.search || '';
    const classId = req.query.classId || '';
    const locationId = req.query.locationId || '';
    const isEmpty = req.query.isEmpty === 'true';
    
    // Validate parameters
    if (page < 1 || limit < 1 || limit > 100) {
      return next(createBadRequestError('Invalid pagination parameters'));
    }
    
    // Get boxes from storage service
    const boxesCollection = global.storageService.getCollection('boxes');
    if (!boxesCollection) {
      return next(createNotFoundError('Boxes collection not found'));
    }
    
    // Filter boxes based on query parameters
    let filteredBoxes = Array.from(boxesCollection.values());
    
    // Apply search filter (case-insensitive)
    if (search) {
      const searchLower = search.toLowerCase();
      filteredBoxes = filteredBoxes.filter(box => {
        // Search in serial number
        if (box.sn && box.sn[0] && box.sn[0].toLowerCase().includes(searchLower)) {
          return true;
        }
        
        // Search in barcodes
        if (box.brc && Array.isArray(box.brc) && box.brc.some(bc => bc.toLowerCase().includes(searchLower))) {
          return true;
        }
        
        // Search in description
        if (box.desc && Array.isArray(box.desc) && box.desc.some(d => d && d.toLowerCase().includes(searchLower))) {
          return true;
        }
        
        return false;
      });
    }
    
    // Apply class filter
    if (classId) {
      filteredBoxes = filteredBoxes.filter(box => box.cl === classId);
    }
    
    // Apply location filter
    if (locationId) {
      filteredBoxes = filteredBoxes.filter(box => {
        return box.loc && 
               Array.isArray(box.loc) && 
               box.loc.length > 0 && 
               box.loc[box.loc.length - 1][0] === locationId;
      });
    }
    
    // Apply empty filter
    if (isEmpty) {
      filteredBoxes = filteredBoxes.filter(box => {
        return !box.cont || !Array.isArray(box.cont) || box.cont.length === 0;
      });
    }
    
    // Sort boxes by timestamp (newest first)
    filteredBoxes.sort((a, b) => {
      const aTime = a.sn && a.sn.length > 1 ? a.sn[1] : 0;
      const bTime = b.sn && b.sn.length > 1 ? b.sn[1] : 0;
      return bTime - aTime;
    });
    
    // Apply pagination
    const startIndex = (page - 1) * limit;
    const endIndex = page * limit;
    const paginatedBoxes = filteredBoxes.slice(startIndex, endIndex);
    
    // Format boxes for response
    const formattedBoxes = paginatedBoxes.map(box => formatBox(box));
    
    // Return paginated results
    return res.status(200).json({
      boxes: formattedBoxes,
      pagination: {
        total: filteredBoxes.length,
        page,
        limit,
        pages: Math.ceil(filteredBoxes.length / limit)
      }
    });
  } catch (error) {
    logger.error(`Error getting boxes: ${error.message}`);
    next(error);
  }
}

/**
 * Get box by serial number
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function getBoxBySerialNumber(req, res, next) {
  try {
    const { serialNumber } = req.params;
    
    // Get box from storage
    const box = global.storageService.getItem('boxes', serialNumber);
    
    if (!box) {
      return next(createNotFoundError(`Box with serial number ${serialNumber} not found`));
    }
    
    // Format box for response
    const formattedBox = formatBox(box);
    
    // Return box
    return res.status(200).json({
      box: formattedBox
    });
  } catch (error) {
    logger.error(`Error getting box by serial number: ${error.message}`);
    next(error);
  }
}

/**
 * Create a new box
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function createBox(req, res, next) {
  try {
    const { className, description } = req.body;
    
    // Generate a new serial number
    const serialNumber = global.storageService.generateSerialNumber('b');
    
    // Create box model
    const Box = require('../models/box');
    const newBox = new Box(serialNumber, description);
    
    if (className) {
      newBox.cl = className;
    }
    
    // Save to storage
    const saved = global.storageService.saveItem('boxes', serialNumber, newBox);
    
    if (!saved) {
      return next(createBadRequestError('Failed to create box'));
    }
    
    // Format box for response
    const formattedBox = formatBox(newBox);
    
    // Return created box
    return res.status(201).json({
      message: 'Box created successfully',
      box: formattedBox
    });
  } catch (error) {
    logger.error(`Error creating box: ${error.message}`);
    next(error);
  }
}

/**
 * Update a box
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function updateBox(req, res, next) {
  try {
    const { serialNumber } = req.params;
    const { className, description, multiplier } = req.body;
    
    // Get box from storage
    const box = global.storageService.getItem('boxes', serialNumber);
    
    if (!box) {
      return next(createNotFoundError(`Box with serial number ${serialNumber} not found`));
    }
    
    // Update fields if provided
    if (className) {
      box.cl = className;
    }
    
    if (description) {
      // Add new description at the beginning of the array
      if (!box.desc || !Array.isArray(box.desc)) {
        box.desc = [];
      }
      box.desc.unshift(description);
      
      // Limit the number of descriptions to keep
      if (box.desc.length > 5) {
        box.desc = box.desc.slice(0, 5);
      }
    }
    
    if (multiplier) {
      box.setMultiplier(parseInt(multiplier));
    }
    
    // Save to storage
    const saved = global.storageService.saveItem('boxes', serialNumber, box);
    
    if (!saved) {
      return next(createBadRequestError('Failed to update box'));
    }
    
    // Format box for response
    const formattedBox = formatBox(box);
    
    // Return updated box
    return res.status(200).json({
      message: 'Box updated successfully',
      box: formattedBox
    });
  } catch (error) {
    logger.error(`Error updating box: ${error.message}`);
    next(error);
  }
}

/**
 * Update box location
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function updateBoxLocation(req, res, next) {
  try {
    const { serialNumber } = req.params;
    const { locationId } = req.body;
    
    if (!locationId) {
      return next(createBadRequestError('Location ID is required'));
    }
    
    // Get box from storage
    const box = global.storageService.getItem('boxes', serialNumber);
    
    if (!box) {
      return next(createNotFoundError(`Box with serial number ${serialNumber} not found`));
    }
    
    // Validate location
    const location = global.storageService.getItem('places', locationId);
    
    if (!location) {
      return next(createNotFoundError(`Location with ID ${locationId} not found`));
    }
    
    // Update box location
    box.setLocation(locationId);
    
    // Save to storage
    const saved = global.storageService.saveItem('boxes', serialNumber, box);
    
    if (!saved) {
      return next(createBadRequestError('Failed to update box location'));
    }
    
    // Also update location for all items in the box
    if (box.cont && Array.isArray(box.cont)) {
      for (const itemEntry of box.cont) {
        if (Array.isArray(itemEntry) && itemEntry.length > 0) {
          const itemId = itemEntry[0];
          global.storageService.updateItemLocation(itemId, serialNumber);
        }
      }
    }
    
    // Format box for response
    const formattedBox = formatBox(box);
    
    // Return updated box
    return res.status(200).json({
      message: 'Box location updated successfully',
      box: formattedBox
    });
  } catch (error) {
    logger.error(`Error updating box location: ${error.message}`);
    next(error);
  }
}

/**
 * Add item to box
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function addItemToBox(req, res, next) {
  try {
    const { serialNumber } = req.params;
    const { itemId } = req.body;
    
    if (!itemId) {
      return next(createBadRequestError('Item ID is required'));
    }
    
    // Get box from storage
    const box = global.storageService.getItem('boxes', serialNumber);
    
    if (!box) {
      return next(createNotFoundError(`Box with serial number ${serialNumber} not found`));
    }
    
    // Get item
    const item = global.storageService.getItem('items', itemId);
    
    if (!item) {
      return next(createNotFoundError(`Item with ID ${itemId} not found`));
    }
    
    // Check if item is already in the box
    if (box.hasItem(itemId)) {
      return res.status(200).json({
        message: 'Item is already in the box',
        box: formatBox(box)
      });
    }
    
    // Add item to box
    box.addItem(itemId);
    
    // Update item location
    item.setLocation(serialNumber);
    
    // Save both box and item
    const boxSaved = global.storageService.saveItem('boxes', serialNumber, box);
    const itemSaved = global.storageService.saveItem('items', itemId, item);
    
    if (!boxSaved || !itemSaved) {
      return next(createBadRequestError('Failed to add item to box'));
    }
    
    // Format box for response
    const formattedBox = formatBox(box);
    
    // Return updated box
    return res.status(200).json({
      message: 'Item added to box successfully',
      box: formattedBox
    });
  } catch (error) {
    logger.error(`Error adding item to box: ${error.message}`);
    next(error);
  }
}

/**
 * Remove item from box
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function removeItemFromBox(req, res, next) {
  try {
    const { serialNumber, itemId } = req.params;
    
    // Get box from storage
    const box = global.storageService.getItem('boxes', serialNumber);
    
    if (!box) {
      return next(createNotFoundError(`Box with serial number ${serialNumber} not found`));
    }
    
    // Check if item is in the box
    if (!box.hasItem(itemId)) {
      return next(createNotFoundError(`Item with ID ${itemId} not found in the box`));
    }
    
    // Remove item from box
    box.removeItem(itemId);
    
    // Save box
    const saved = global.storageService.saveItem('boxes', serialNumber, box);
    
    if (!saved) {
      return next(createBadRequestError('Failed to remove item from box'));
    }
    
    // Update item location if the item exists
    const item = global.storageService.getItem('items', itemId);
    if (item) {
      // Update item's location (set to unknown or a default location)
      item.setLocation('p000000000000000001'); // Default "unknown" location
      global.storageService.saveItem('items', itemId, item);
    }
    
    // Format box for response
    const formattedBox = formatBox(box);
    
    // Return updated box
    return res.status(200).json({
      message: 'Item removed from box successfully',
      box: formattedBox
    });
  } catch (error) {
    logger.error(`Error removing item from box: ${error.message}`);
    next(error);
  }
}

/**
 * Add barcode to box
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
    
    // Get box from storage
    const box = global.storageService.getItem('boxes', serialNumber);
    
    if (!box) {
      return next(createNotFoundError(`Box with serial number ${serialNumber} not found`));
    }
    
    // Add barcode
    const added = box.addBarcode(barcode);
    
    if (!added) {
      return next(createBadRequestError('Failed to add barcode to box'));
    }
    
    // Save to storage
    const saved = global.storageService.saveItem('boxes', serialNumber, box);
    
    if (!saved) {
      return next(createBadRequestError('Failed to save box with new barcode'));
    }
    
    // Format box for response
    const formattedBox = formatBox(box);
    
    // Return updated box
    return res.status(200).json({
      message: 'Barcode added successfully',
      box: formattedBox
    });
  } catch (error) {
    logger.error(`Error adding barcode to box: ${error.message}`);
    next(error);
  }
}

/**
 * Get box contents
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function getBoxContents(req, res, next) {
  try {
    const { serialNumber } = req.params;
    
    // Get box from storage
    const box = global.storageService.getItem('boxes', serialNumber);
    
    if (!box) {
      return next(createNotFoundError(`Box with serial number ${serialNumber} not found`));
    }
    
    // Get item IDs
    const itemIds = box.getItems();
    
    if (!itemIds || itemIds.length === 0) {
      return res.status(200).json({
        box: formatBox(box),
        items: []
      });
    }
    
    // Get item objects
    const items = [];
    for (const itemId of itemIds) {
      const item = global.storageService.getItem('items', itemId);
      if (item) {
        // Format item for API response
        const formattedItem = formatItem(item);
        items.push(formattedItem);
      }
    }
    
    // Return box and its contents
    return res.status(200).json({
      box: formatBox(box),
      items: items
    });
  } catch (error) {
    logger.error(`Error getting box contents: ${error.message}`);
    next(error);
  }
}

/**
 * Get box history
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function getBoxHistory(req, res, next) {
  try {
    const { serialNumber } = req.params;
    
    // Get box from storage
    const box = global.storageService.getItem('boxes', serialNumber);
    
    if (!box) {
      return next(createNotFoundError(`Box with serial number ${serialNumber} not found`));
    }
    
    // Get options
    const limit = parseInt(req.query.limit) || 100;
    const offset = parseInt(req.query.offset) || 0;
    const action = req.query.action || null;
    const startTime = req.query.startTime ? parseInt(req.query.startTime) : 0;
    const endTime = req.query.endTime ? parseInt(req.query.endTime) : Math.floor(Date.now() / 1000);
    
    // Get history
    const history = await global.historyService.getHistory('boxes', serialNumber, {
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
    logger.error(`Error getting box history: ${error.message}`);
    next(error);
  }
}

/**
 * Find box by barcode
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function findBoxByBarcode(req, res, next) {
  try {
    const { barcode } = req.params;
    
    // Get all boxes
    const boxesCollection = global.storageService.getCollection('boxes');
    
    if (!boxesCollection) {
      return next(createNotFoundError('Boxes collection not found'));
    }
    
    // Find boxes with matching barcode
    const boxes = Array.from(boxesCollection.values()).filter(box => {
      return box.brc && Array.isArray(box.brc) && box.brc.includes(barcode);
    });
    
    if (boxes.length === 0) {
      return next(createNotFoundError(`No boxes found with barcode ${barcode}`));
    }
    
    // Format boxes for response
    const formattedBoxes = boxes.map(box => formatBox(box));
    
    // Return boxes
    return res.status(200).json({
      boxes: formattedBoxes
    });
  } catch (error) {
    logger.error(`Error finding box by barcode: ${error.message}`);
    next(error);
  }
}

/**
 * Format box for API response
 * @param {Object} box - Raw box object
 * @returns {Object} Formatted box
 */
function formatBox(box) {
  // Create a deep copy of the box
  const formatted = JSON.parse(JSON.stringify(box));
  
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
  
  // Format contents
  if (formatted.cont && Array.isArray(formatted.cont)) {
    formatted.contents = formatted.cont.map(cont => {
      return {
        id: cont[0],
        timestamp: cont[1]
      };
    });
    
    // Remove original cont field
    delete formatted.cont;
  }
  
  // Format incoming history
  if (formatted.in && Array.isArray(formatted.in)) {
    formatted.incoming = formatted.in.map(inc => {
      return {
        id: inc[0],
        timestamp: inc[1]
      };
    });
    
    // Remove original in field
    delete formatted.in;
  }
  
  // Format outgoing history
  if (formatted.out && Array.isArray(formatted.out)) {
    formatted.outgoing = formatted.out.map(out => {
      return {
        id: out[0],
        timestamp: out[1]
      };
    });
    
    // Remove original out field
    delete formatted.out;
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
  
  // Format multiplier
  if (formatted.mult && Array.isArray(formatted.mult) && formatted.mult.length > 0 && formatted.mult[0].length > 0) {
    formatted.multiplier = formatted.mult[0][0];
    delete formatted.mult;
  }
  
  // Format mass measurements
  if (formatted.mas && Array.isArray(formatted.mas)) {
    formatted.mass = formatted.mas;
    delete formatted.mas;
  }
  
  // Format size measurements
  if (formatted.siz && Array.isArray(formatted.siz)) {
    formatted.size = formatted.siz;
    delete formatted.siz;
  }
  
  // Rename cl field to class_id
  if (formatted.cl) {
    formatted.class_id = formatted.cl;
    delete formatted.cl;
  }
  
  return formatted;
}

/**
 * Format item for API response (simplified version for box contents)
 * @param {Object} item - Raw item object
 * @returns {Object} Formatted item
 */
function formatItem(item) {
  // Create a deep copy of the item
  const formatted = JSON.parse(JSON.stringify(item));
  
  // Format creation timestamp
  if (formatted.sn && formatted.sn.length > 1) {
    formatted.created_at = new Date(formatted.sn[1] * 1000);
    formatted.serial_number = formatted.sn[0];
  }
  
  // Format condition
  if (formatted.cond && Array.isArray(formatted.cond) && formatted.cond.length > 0) {
    formatted.condition = formatted.cond[0];
  }
  
  // Rename cl field to class_id
  if (formatted.cl) {
    formatted.class_id = formatted.cl;
  }
  
  return formatted;
}

module.exports = {
  getAllBoxes,
  getBoxBySerialNumber,
  createBox,
  updateBox,
  updateBoxLocation,
  addItemToBox,
  removeItemFromBox,
  addBarcode,
  getBoxContents,
  getBoxHistory,
  findBoxByBarcode
};