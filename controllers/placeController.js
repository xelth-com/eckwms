// controllers/placeController.js
const logger = require('../utils/logging');
const { ApiError, createNotFoundError, createBadRequestError } = require('../middleware/errorHandler');

/**
 * Get all places with pagination and filtering
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function getAllPlaces(req, res, next) {
  try {
    // Extract query parameters
    const page = parseInt(req.query.page) || 1;
    const limit = parseInt(req.query.limit) || 20;
    const search = req.query.search || '';
    const classId = req.query.classId || '';
    const parent = req.query.parent || '';
    
    // Validate parameters
    if (page < 1 || limit < 1 || limit > 100) {
      return next(createBadRequestError('Invalid pagination parameters'));
    }
    
    // Get places from storage service
    const placesCollection = global.storageService.getCollection('places');
    if (!placesCollection) {
      return next(createNotFoundError('Places collection not found'));
    }
    
    // Filter places based on query parameters
    let filteredPlaces = Array.from(placesCollection.values());
    
    // Apply search filter (case-insensitive)
    if (search) {
      const searchLower = search.toLowerCase();
      filteredPlaces = filteredPlaces.filter(place => {
        // Search in serial number
        if (place.sn && place.sn[0] && place.sn[0].toLowerCase().includes(searchLower)) {
          return true;
        }
        
        // Search in description
        if (place.desc && Array.isArray(place.desc) && place.desc.some(d => d && d.toLowerCase().includes(searchLower))) {
          return true;
        }
        
        return false;
      });
    }
    
    // Apply class filter
    if (classId) {
      filteredPlaces = filteredPlaces.filter(place => place.cl === classId);
    }
    
    // Apply parent filter
    if (parent) {
      filteredPlaces = filteredPlaces.filter(place => {
        return place.par === parent;
      });
    }
    
    // Sort places by serial number
    filteredPlaces.sort((a, b) => {
      if (a.sn && a.sn[0] && b.sn && b.sn[0]) {
        return a.sn[0].localeCompare(b.sn[0]);
      }
      return 0;
    });
    
    // Apply pagination
    const startIndex = (page - 1) * limit;
    const endIndex = page * limit;
    const paginatedPlaces = filteredPlaces.slice(startIndex, endIndex);
    
    // Format places for response
    const formattedPlaces = paginatedPlaces.map(place => formatPlace(place));
    
    // Return paginated results
    return res.status(200).json({
      places: formattedPlaces,
      pagination: {
        total: filteredPlaces.length,
        page,
        limit,
        pages: Math.ceil(filteredPlaces.length / limit)
      }
    });
  } catch (error) {
    logger.error(`Error getting places: ${error.message}`);
    next(error);
  }
}

/**
 * Get place by serial number
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function getPlaceBySerialNumber(req, res, next) {
  try {
    const { serialNumber } = req.params;
    
    // Get place from storage
    const place = global.storageService.getItem('places', serialNumber);
    
    if (!place) {
      return next(createNotFoundError(`Place with serial number ${serialNumber} not found`));
    }
    
    // Format place for response
    const formattedPlace = formatPlace(place);
    
    // Return place
    return res.status(200).json({
      place: formattedPlace
    });
  } catch (error) {
    logger.error(`Error getting place by serial number: ${error.message}`);
    next(error);
  }
}

/**
 * Create a new place
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function createPlace(req, res, next) {
  try {
    const { className, description, parentId } = req.body;
    
    // Generate a new serial number
    const serialNumber = global.storageService.generateSerialNumber('p');
    
    // Create place model (using Betruger since there is no specific place model)
    const Betruger = require('../models/betruger');
    const newPlace = new Betruger();
    
    // Set basic properties
    newPlace.sn = [serialNumber, Math.floor(Date.now() / 1000)];
    
    if (className) {
      newPlace.cl = className;
    }
    
    if (description) {
      newPlace.desc = [description];
    }
    
    if (parentId) {
      // Verify parent place exists
      const parent = global.storageService.getItem('places', parentId);
      if (!parent) {
        return next(createNotFoundError(`Parent place with ID ${parentId} not found`));
      }
      
      newPlace.par = parentId;
    }
    
    // Initialize content array
    newPlace.cont = [];
    
    // Save to storage
    const saved = global.storageService.saveItem('places', serialNumber, newPlace);
    
    if (!saved) {
      return next(createBadRequestError('Failed to create place'));
    }
    
    // Format place for response
    const formattedPlace = formatPlace(newPlace);
    
    // Return created place
    return res.status(201).json({
      message: 'Place created successfully',
      place: formattedPlace
    });
  } catch (error) {
    logger.error(`Error creating place: ${error.message}`);
    next(error);
  }
}

/**
 * Update a place
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function updatePlace(req, res, next) {
  try {
    const { serialNumber } = req.params;
    const { className, description, parentId } = req.body;
    
    // Get place from storage
    const place = global.storageService.getItem('places', serialNumber);
    
    if (!place) {
      return next(createNotFoundError(`Place with serial number ${serialNumber} not found`));
    }
    
    // Update fields if provided
    if (className) {
      place.cl = className;
    }
    
    if (description) {
      if (!place.desc || !Array.isArray(place.desc)) {
        place.desc = [];
      }
      place.desc.unshift(description);
      
      // Limit the number of descriptions to keep
      if (place.desc.length > 5) {
        place.desc = place.desc.slice(0, 5);
      }
    }
    
    if (parentId) {
      // Verify parent place exists
      const parent = global.storageService.getItem('places', parentId);
      if (!parent) {
        return next(createNotFoundError(`Parent place with ID ${parentId} not found`));
      }
      
      // Make sure it's not setting itself as parent
      if (parentId === serialNumber) {
        return next(createBadRequestError('Cannot set place as its own parent'));
      }
      
      // Make sure it doesn't create a circular reference
      if (isCircularReference(parentId, serialNumber)) {
        return next(createBadRequestError('Cannot create circular reference in place hierarchy'));
      }
      
      place.par = parentId;
    }
    
    // Save to storage
    const saved = global.storageService.saveItem('places', serialNumber, place);
    
    if (!saved) {
      return next(createBadRequestError('Failed to update place'));
    }
    
    // Format place for response
    const formattedPlace = formatPlace(place);
    
    // Return updated place
    return res.status(200).json({
      message: 'Place updated successfully',
      place: formattedPlace
    });
  } catch (error) {
    logger.error(`Error updating place: ${error.message}`);
    next(error);
  }
}

/**
 * Get place contents
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function getPlaceContents(req, res, next) {
  try {
    const { serialNumber } = req.params;
    
    // Get place from storage
    const place = global.storageService.getItem('places', serialNumber);
    
    if (!place) {
      return next(createNotFoundError(`Place with serial number ${serialNumber} not found`));
    }
    
    // Check if place has contents
    if (!place.cont || !Array.isArray(place.cont)) {
      return res.status(200).json({
        place: formatPlace(place),
        contents: {
          items: [],
          boxes: []
        }
      });
    }
    
    // Prepare content arrays
    const items = [];
    const boxes = [];
    
    // Process each content entry
    for (const entry of place.cont) {
      if (!Array.isArray(entry) || entry.length < 1) continue;
      
      const contentId = entry[0];
      
      // Check if it's an item
      if (contentId.startsWith('i')) {
        const item = global.storageService.getItem('items', contentId);
        if (item) {
          items.push(formatItem(item));
        }
      } 
      // Check if it's a box
      else if (contentId.startsWith('b')) {
        const box = global.storageService.getItem('boxes', contentId);
        if (box) {
          boxes.push(formatBox(box));
        }
      }
    }
    
    // Return place with contents
    return res.status(200).json({
      place: formatPlace(place),
      contents: {
        items,
        boxes
      }
    });
  } catch (error) {
    logger.error(`Error getting place contents: ${error.message}`);
    next(error);
  }
}

/**
 * Get place hierarchy (parent chain)
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function getPlaceHierarchy(req, res, next) {
  try {
    const { serialNumber } = req.params;
    
    // Get place from storage
    const place = global.storageService.getItem('places', serialNumber);
    
    if (!place) {
      return next(createNotFoundError(`Place with serial number ${serialNumber} not found`));
    }
    
    // Build hierarchy chain
    const hierarchyChain = [];
    let currentPlace = place;
    let maxDepth = 10; // Prevent infinite loops
    
    while (currentPlace && maxDepth > 0) {
      hierarchyChain.unshift(formatPlace(currentPlace));
      
      if (!currentPlace.par) break;
      
      currentPlace = global.storageService.getItem('places', currentPlace.par);
      maxDepth--;
    }
    
    // Return hierarchy
    return res.status(200).json({
      hierarchy: hierarchyChain
    });
  } catch (error) {
    logger.error(`Error getting place hierarchy: ${error.message}`);
    next(error);
  }
}

/**
 * Get children places
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function getPlaceChildren(req, res, next) {
  try {
    const { serialNumber } = req.params;
    
    // Get place from storage
    const place = global.storageService.getItem('places', serialNumber);
    
    if (!place) {
      return next(createNotFoundError(`Place with serial number ${serialNumber} not found`));
    }
    
    // Get all places
    const placesCollection = global.storageService.getCollection('places');
    if (!placesCollection) {
      return next(createNotFoundError('Places collection not found'));
    }
    
    // Find child places
    const children = Array.from(placesCollection.values()).filter(p => {
      return p.par === serialNumber;
    });
    
    // Format places for response
    const formattedChildren = children.map(place => formatPlace(place));
    
    // Return children
    return res.status(200).json({
      children: formattedChildren
    });
  } catch (error) {
    logger.error(`Error getting place children: ${error.message}`);
    next(error);
  }
}

/**
 * Add items to a place
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function addItemsToPlace(req, res, next) {
  try {
    const { serialNumber } = req.params;
    const { items } = req.body;
    
    // Validate request
    if (!items || !Array.isArray(items) || items.length === 0) {
      return next(createBadRequestError('Items array is required'));
    }
    
    // Get place from storage
    const place = global.storageService.getItem('places', serialNumber);
    
    if (!place) {
      return next(createNotFoundError(`Place with serial number ${serialNumber} not found`));
    }
    
    // Initialize content array if it doesn't exist
    if (!place.cont || !Array.isArray(place.cont)) {
      place.cont = [];
    }
    
    // Add items to place
    const timestamp = Math.floor(Date.now() / 1000);
    const addedItems = [];
    
    for (const itemId of items) {
      // Check if item exists
      const item = global.storageService.getItem('items', itemId);
      if (!item) {
        logger.warn(`Item ${itemId} not found, skipping`);
        continue;
      }
      
      // Check if item is already in the place
      const existingItemIndex = place.cont.findIndex(entry => 
        Array.isArray(entry) && entry.length > 0 && entry[0] === itemId
      );
      
      if (existingItemIndex >= 0) {
        // Update timestamp
        place.cont[existingItemIndex][1] = timestamp;
      } else {
        // Add new item
        place.cont.push([itemId, timestamp]);
        addedItems.push(itemId);
      }
      
      // Update item location
      item.setLocation(serialNumber);
      global.storageService.saveItem('items', itemId, item);
    }
    
    // Save place to storage
    const saved = global.storageService.saveItem('places', serialNumber, place);
    
    if (!saved) {
      return next(createBadRequestError('Failed to add items to place'));
    }
    
    // Return updated place
    return res.status(200).json({
      message: `Added ${addedItems.length} items to place`,
      place: formatPlace(place)
    });
  } catch (error) {
    logger.error(`Error adding items to place: ${error.message}`);
    next(error);
  }
}

/**
 * Add boxes to a place
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function addBoxesToPlace(req, res, next) {
  try {
    const { serialNumber } = req.params;
    const { boxes } = req.body;
    
    // Validate request
    if (!boxes || !Array.isArray(boxes) || boxes.length === 0) {
      return next(createBadRequestError('Boxes array is required'));
    }
    
    // Get place from storage
    const place = global.storageService.getItem('places', serialNumber);
    
    if (!place) {
      return next(createNotFoundError(`Place with serial number ${serialNumber} not found`));
    }
    
    // Initialize content array if it doesn't exist
    if (!place.cont || !Array.isArray(place.cont)) {
      place.cont = [];
    }
    
    // Add boxes to place
    const timestamp = Math.floor(Date.now() / 1000);
    const addedBoxes = [];
    
    for (const boxId of boxes) {
      // Check if box exists
      const box = global.storageService.getItem('boxes', boxId);
      if (!box) {
        logger.warn(`Box ${boxId} not found, skipping`);
        continue;
      }
      
      // Check if box is already in the place
      const existingBoxIndex = place.cont.findIndex(entry => 
        Array.isArray(entry) && entry.length > 0 && entry[0] === boxId
      );
      
      if (existingBoxIndex >= 0) {
        // Update timestamp
        place.cont[existingBoxIndex][1] = timestamp;
      } else {
        // Add new box
        place.cont.push([boxId, timestamp]);
        addedBoxes.push(boxId);
      }
      
      // Update box location
      box.setLocation(serialNumber);
      global.storageService.saveItem('boxes', boxId, box);
    }
    
    // Save place to storage
    const saved = global.storageService.saveItem('places', serialNumber, place);
    
    if (!saved) {
      return next(createBadRequestError('Failed to add boxes to place'));
    }
    
    // Return updated place
    return res.status(200).json({
      message: `Added ${addedBoxes.length} boxes to place`,
      place: formatPlace(place)
    });
  } catch (error) {
    logger.error(`Error adding boxes to place: ${error.message}`);
    next(error);
  }
}

/**
 * Remove item from place
 * @param {Object} req - Express request object
 * @param {Object} res - Express response object
 * @param {Function} next - Express next function
 */
async function removeItemFromPlace(req, res, next) {
  try {
    const { serialNumber, itemId } = req.params;
    
    // Get place from storage
    const place = global.storageService.getItem('places', serialNumber);
    
    if (!place) {
      return next(createNotFoundError(`Place with serial number ${serialNumber} not found`));
    }
    
    // Check if place has contents
    if (!place.cont || !Array.isArray(place.cont) || place.cont.length === 0) {
      return next(createNotFoundError(`Item with ID ${itemId} not found in place`));
    }
    
    // Find item in place contents
    const itemIndex = place.cont.findIndex(entry => 
      Array.isArray(entry) && entry.length > 0 && entry[0] === itemId
    );
    
    if (itemIndex === -1) {
      return next(createNotFoundError(`Item with ID ${itemId} not found in place`));
    }
    
    // Remove item from place
    place.cont.splice(itemIndex, 1);
    
    // Save place to storage
    const saved = global.storageService.saveItem('places', serialNumber, place);
    
    if (!saved) {
      return next(createBadRequestError('Failed to remove item from place'));
    }
    
    // Return updated place
    return res.status(200).json({
      message: 'Item removed from place successfully',
      place: formatPlace(place)
    });
  } catch (error) {
    logger.error(`Error removing item from place: ${error.message}`);
    next(error);
  }
}

/**
 * Format place for API response
 * @param {Object} place - Raw place object
 * @returns {Object} Formatted place
 */
function formatPlace(place) {
  // Create a deep copy of the place
  const formatted = JSON.parse(JSON.stringify(place));
  
  // Format creation timestamp
  if (formatted.sn && formatted.sn.length > 1) {
    formatted.created_at = new Date(formatted.sn[1] * 1000);
    formatted.serial_number = formatted.sn[0];
    delete formatted.sn;
  }
  
  // Format description
  if (formatted.desc && Array.isArray(formatted.desc)) {
    formatted.description = formatted.desc;
    delete formatted.desc;
  }
  
  // Rename cl field to class_id
  if (formatted.cl) {
    formatted.class_id = formatted.cl;
    delete formatted.cl;
  }
  
  // Format parent reference
  if (formatted.par) {
    formatted.parent_id = formatted.par;
    delete formatted.par;
  }
  
  // Format content counts
  if (formatted.cont && Array.isArray(formatted.cont)) {
    const itemCount = formatted.cont.filter(entry => 
      Array.isArray(entry) && entry.length > 0 && entry[0].startsWith('i')
    ).length;
    
    const boxCount = formatted.cont.filter(entry => 
      Array.isArray(entry) && entry.length > 0 && entry[0].startsWith('b')
    ).length;
    
    formatted.content_count = {
      items: itemCount,
      boxes: boxCount,
      total: formatted.cont.length
    };
    
    // Remove detailed content array from response
    delete formatted.cont;
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
  
  // Format class
  if (box.cl) {
    formatted.class_id = box.cl;
  }
  
  // Format content count
  if (box.cont && Array.isArray(box.cont)) {
    formatted.item_count = box.cont.length;
  } else {
    formatted.item_count = 0;
  }
  
  return formatted;
}

/**
 * Check if setting a parent would create a circular reference
 * @param {string} parentId - Proposed parent ID
 * @param {string} placeId - Current place ID
 * @returns {boolean} True if circular reference would be created
 */
function isCircularReference(parentId, placeId) {
  let currentId = parentId;
  let maxDepth = 10; // Prevent infinite loops
  
  while (currentId && maxDepth > 0) {
    if (currentId === placeId) {
      return true; // Circular reference found
    }
    
    const parent = global.storageService.getItem('places', currentId);
    if (!parent || !parent.par) {
      return false; // No more parents, no circular reference
    }
    
    currentId = parent.par;
    maxDepth--;
  }
  
  return false;
}

module.exports = {
  getAllPlaces,
  getPlaceBySerialNumber,
  createPlace,
  updatePlace,
  getPlaceContents,
  getPlaceHierarchy,
  getPlaceChildren,
  addItemsToPlace,
  addBoxesToPlace,
  removeItemFromPlace
};