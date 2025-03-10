// models/order.js
const Betruger = require('./betruger');
const logger = require('../utils/logging');

/**
 * Order model for RMA and purchase management
 * @class Order
 * @extends Betruger
 */
class Order extends Betruger {
  /**
   * Create a new Order
   * @param {string} serialNumber - Order serial number
   */
  constructor(serialNumber) {
    super();
    
    this.sn = [serialNumber, Math.floor(Date.now() / 1000)];
    this.cont = []; // Contents (items/boxes in the order)
    this.decl = []; // Declarations from customer
    this.st = 'pending'; // Status (pending, processing, shipping, completed, cancelled)
    this.notes = []; // Internal notes
  }
  
  /**
   * Add an item to the order
   * @param {string} itemId - Item serial number
   * @returns {boolean} Success status
   */
  addItem(itemId) {
    if (!itemId) return false;
    
    const timestamp = Math.floor(Date.now() / 1000);
    
    // Check if item is already in the order
    const existingIndex = this.cont.findIndex(entry => {
      if (Array.isArray(entry) && entry.length > 0) {
        return entry[0] === itemId;
      }
      return false;
    });
    
    if (existingIndex >= 0) {
      // Update timestamp
      this.cont[existingIndex][1] = timestamp;
      return true;
    }
    
    // Add new item
    this.cont.push([itemId, timestamp]);
    return true;
  }
  
  /**
   * Remove an item from the order
   * @param {string} itemId - Item serial number
   * @returns {boolean} Success status
   */
  removeItem(itemId) {
    if (!itemId || !this.cont || !Array.isArray(this.cont)) return false;
    
    // Find item
    const index = this.cont.findIndex(entry => {
      if (Array.isArray(entry) && entry.length > 0) {
        return entry[0] === itemId;
      }
      return false;
    });
    
    if (index >= 0) {
      // Remove item
      this.cont.splice(index, 1);
      return true;
    }
    
    return false;
  }
  
  /**
   * Add a box to the order
   * @param {string} boxId - Box serial number
   * @returns {boolean} Success status
   */
  addBox(boxId) {
    if (!boxId) return false;
    
    const timestamp = Math.floor(Date.now() / 1000);
    
    // Check if box is already in the order
    const existingIndex = this.cont.findIndex(entry => {
      if (Array.isArray(entry) && entry.length > 0) {
        return entry[0] === boxId;
      }
      return false;
    });
    
    if (existingIndex >= 0) {
      // Update timestamp
      this.cont[existingIndex][1] = timestamp;
      return true;
    }
    
    // Add new box
    this.cont.push([boxId, timestamp]);
    return true;
  }
  
  /**
   * Add a declaration to the order
   * @param {string} serialNumber - Serial number declared by customer
   * @param {string} description - Issue description
   * @returns {boolean} Success status
   */
  addDeclaration(serialNumber, description) {
    if (!serialNumber || !description) return false;
    
    // Add declaration
    this.decl.push([serialNumber, description]);
    return true;
  }
  
  /**
   * Update order status
   * @param {string} status - Order status
   * @returns {boolean} Success status
   */
  updateStatus(status) {
    const validStatuses = ['pending', 'processing', 'shipping', 'completed', 'cancelled'];
    
    if (!status || !validStatuses.includes(status)) {
      return false;
    }
    
    this.st = status;
    return true;
  }
  
  /**
   * Add a note to the order
   * @param {string} note - Note text
   * @param {string} userId - User ID who added the note
   * @returns {boolean} Success status
   */
  addNote(note, userId) {
    if (!note) return false;
    
    const timestamp = Math.floor(Date.now() / 1000);
    
    if (!this.notes) {
      this.notes = [];
    }
    
    this.notes.push({
      text: note,
      user: userId || 'system',
      timestamp
    });
    
    return true;
  }
  
  /**
   * Get order status
   * @returns {string} Order status
   */
  getStatus() {
    return this.st || 'pending';
  }
  
  /**
   * Get order contents (items and boxes)
   * @returns {Array} Array of content IDs
   */
  getContents() {
    if (!this.cont || !Array.isArray(this.cont)) {
      return [];
    }
    
    return this.cont.map(entry => {
      if (Array.isArray(entry) && entry.length > 0) {
        return entry[0];
      }
      return null;
    }).filter(id => id !== null);
  }
  
  /**
   * Get order declarations
   * @returns {Array} Array of declarations
   */
  getDeclarations() {
    if (!this.decl || !Array.isArray(this.decl)) {
      return [];
    }
    
    return this.decl.map(decl => {
      if (Array.isArray(decl) && decl.length >= 2) {
        return {
          serialNumber: decl[0],
          description: decl[1]
        };
      }
      return null;
    }).filter(decl => decl !== null);
  }
}

module.exports = Order;