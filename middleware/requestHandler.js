// middleware/requestHandler.js
const logger = require('../utils/logging');

/**
 * Middleware for handling case sensitivity in IDs and references
 */
class RequestHandler {
  /**
   * Normalize item IDs in requests to ensure case-insensitive handling
   * @param {Object} req - Express request object
   * @param {Object} res - Express response object
   * @param {Function} next - Express next function
   */
  static normalizeItemIds(req, res, next) {
    try {
      // Normalize item ID in route params
      if (req.params.itemId) {
        const originalId = req.params.itemId;
        const item = global.storageService.getItem('items', originalId);
        
        if (item) {
          // Replace with canonical ID (maintains original case)
          req.params.itemId = item.sn[0];
          req.canonicalItemId = item.sn[0];
        }
      }
      
      // Normalize item ID in request body
      if (req.body.itemId) {
        const originalId = req.body.itemId;
        const item = global.storageService.getItem('items', originalId);
        
        if (item) {
          // Replace with canonical ID (maintains original case)
          req.body.itemId = item.sn[0];
        }
      }
      
      // Normalize item IDs in arrays
      if (req.body.itemIds && Array.isArray(req.body.itemIds)) {
        req.body.itemIds = req.body.itemIds.map(id => {
          const item = global.storageService.getItem('items', id);
          return item ? item.sn[0] : id;
        });
      }
      
      next();
    } catch (error) {
      logger.error(`Error normalizing item IDs: ${error.message}`);
      next();
    }
  }
  
  /**
   * Normalize box IDs in requests
   * @param {Object} req - Express request object
   * @param {Object} res - Express response object
   * @param {Function} next - Express next function
   */
  static normalizeBoxIds(req, res, next) {
    try {
      // Normalize box ID in route params
      if (req.params.boxId) {
        const originalId = req.params.boxId;
        const box = global.storageService.getItem('boxes', originalId);
        
        if (box) {
          // Replace with canonical ID
          req.params.boxId = box.sn[0];
          req.canonicalBoxId = box.sn[0];
        }
      }
      
      // Normalize box ID in request body
      if (req.body.boxId) {
        const originalId = req.body.boxId;
        const box = global.storageService.getItem('boxes', originalId);
        
        if (box) {
          // Replace with canonical ID
          req.body.boxId = box.sn[0];
        }
      }
      
      next();
    } catch (error) {
      logger.error(`Error normalizing box IDs: ${error.message}`);
      next();
    }
  }
  
  /**
   * Track history for entity operations
   * @param {string} entityType - Entity type
   * @param {string} action - Action type
   * @returns {Function} Middleware function
   */
  static trackHistory(entityType, action) {
    return async (req, res, next) => {
      // Store the original 'send' function
      const originalSend = res.send;
      
      // Record information about the current request
      const entityId = req.params.id || 
                       req.params.itemId || 
                       req.params.boxId || 
                       req.params.placeId || 
                       req.params.rmaNumber || 
                       req.body.id;
                       
      // Get the user who made the request
      const userId = req.user?.u || 'anonymous';
      
      // Override the 'send' function
      res.send = function(data) {
        // Only track history for successful operations
        if (res.statusCode >= 200 && res.statusCode < 300) {
          try {
            let responseData;
            
            // Parse response data if it's JSON
            if (typeof data === 'string') {
              try {
                responseData = JSON.parse(data);
              } catch (e) {
                responseData = { rawResponse: data };
              }
            } else {
              responseData = data;
            }
            
            // Record history
            if (global.historyService && entityId) {
              global.historyService.recordHistory(
                entityType,
                entityId,
                action,
                {
                  user: userId,
                  timestamp: Date.now(),
                  requestBody: req.body,
                  responseData,
                  method: req.method,
                  path: req.path
                }
              ).catch(error => {
                logger.error(`Failed to record history: ${error.message}`);
              });
            }
          } catch (error) {
            logger.error(`Error in history tracking middleware: ${error.message}`);
          }
        }
        
        // Call the original 'send' function
        return originalSend.call(this, data);
      };
      
      next();
    };
  }
  
  /**
   * Process container operations (add/remove items to/from boxes)
   * @param {string} operation - Operation type ('add' or 'remove')
   * @returns {Function} Middleware function
   */
  static processContainerOperation(operation) {
    return async (req, res, next) => {
      try {
        const { boxId, itemId } = req.body;
        
        if (!boxId || !itemId) {
          return res.status(400).json({
            message: 'Box ID and item ID are required'
          });
        }
        
        // Get the box and item
        const box = global.storageService.getItem('boxes', boxId);
        const item = global.storageService.getItem('items', itemId);
        
        if (!box) {
          return res.status(404).json({
            message: `Box with ID ${boxId} not found`
          });
        }
        
        if (!item) {
          return res.status(404).json({
            message: `Item with ID ${itemId} not found`
          });
        }
        
        // Process the operation
        if (operation === 'add') {
          // Add item to box
          if (box.addItem(item.sn[0])) {
            // Update the item's location to the box
            item.setLocation(box.sn[0]);
            
            // Save changes
            global.storageService.saveItem('boxes', box.sn[0], box);
            global.storageService.saveItem('items', item.sn[0], item);
            
            // Return success
            req.operationResult = {
              success: true,
              message: `Item ${item.sn[0]} added to box ${box.sn[0]}`,
              box,
              item
            };
          } else {
            return res.status(400).json({
              message: `Failed to add item ${itemId} to box ${boxId}`
            });
          }
        } else if (operation === 'remove') {
          // Remove item from box
          if (box.removeItem(item.sn[0])) {
            // Save changes
            global.storageService.saveItem('boxes', box.sn[0], box);
            
            // Return success
            req.operationResult = {
              success: true,
              message: `Item ${item.sn[0]} removed from box ${box.sn[0]}`,
              box,
              item
            };
          } else {
            return res.status(400).json({
              message: `Item ${itemId} not found in box ${boxId} or removal failed`
            });
          }
        } else {
          return res.status(400).json({
            message: `Invalid operation: ${operation}`
          });
        }
        
        next();
      } catch (error) {
        logger.error(`Error in container operation middleware: ${error.message}`);
        return res.status(500).json({
          message: 'Internal server error during container operation'
        });
      }
    };
  }
}

module.exports = RequestHandler;