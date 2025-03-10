// models/place.js
const Betruger = require('./betruger');
const logger = require('../utils/logging');

/**
 * Place model for location management
 * @class Place
 * @extends Betruger
 */
class Place extends Betruger {
  /**
   * Create a new Place
   * @param {string} serialNumber - Place serial number
   * @param {string} [description] - Place description
   * @param {string} [parentId] - Parent place ID
   */
  constructor(serialNumber, description, parentId) {
    super();
    
    this.sn = [serialNumber, Math.floor(Date.now() / 1000)];
    this.desc = description ? [description] : [];
    this.cont = []; // Contents (items/boxes in this place)
    
    if (parentId) {
      this.par = parentId; // Parent place
    }
  }
  
  /**
   * Add an item to the place
   * @param {string} itemId - Item serial number
   * @returns {boolean} Success status
   */
  addItem(itemId) {
    if (!itemId) return false;
    
    const timestamp = Math.floor(Date.now() / 1000);
    
    // Check if item is already in the place
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
   * Remove an item from the place
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
   * Check if the place contains an item
   * @param {string} itemId - Item serial number
   * @returns {boolean} True if place contains the item
   */
  hasItem(itemId) {
    if (!itemId || !this.cont || !Array.isArray(this.cont)) return false;
    
    return this.cont.some(entry => {
      if (Array.isArray(entry) && entry.length > 0) {
        return entry[0] === itemId;
      }
      return false;
    });
  }
  
  /**
   * Add a box to the place
   * @param {string} boxId - Box serial number
   * @returns {boolean} Success status
   */
  addBox(boxId) {
    if (!boxId) return false;
    
    const timestamp = Math.floor(Date.now() / 1000);
    
    // Check if box is already in the place
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
   * Remove a box from the place
   * @param {string} boxId - Box serial number
   * @returns {boolean} Success status
   */
  removeBox(boxId) {
    if (!boxId || !this.cont || !Array.isArray(this.cont)) return false;
    
    // Find box
    const index = this.cont.findIndex(entry => {
      if (Array.isArray(entry) && entry.length > 0) {
        return entry[0] === boxId;
      }
      return false;
    });
    
    if (index >= 0) {
      // Remove box
      this.cont.splice(index, 1);
      return true;
    }
    
    return false;
  }
  
  /**
   * Get all items in the place
   * @returns {Array} Array of item IDs
   */
  getItemIds() {
    if (!this.cont || !Array.isArray(this.cont)) {
      return [];
    }
    
    // Extract item IDs (items start with 'i')
    return this.cont
      .filter(entry => {
        if (Array.isArray(entry) && entry.length > 0) {
          return entry[0].startsWith('i');
        }
        return false;
      })
      .map(entry => entry[0]);
  }
  
  /**
   * Get all boxes in the place
   * @returns {Array} Array of box IDs
   */
  getBoxIds() {
    if (!this.cont || !Array.isArray(this.cont)) {
      return [];
    }
    
    // Extract box IDs (boxes start with 'b')
    return this.cont
      .filter(entry => {
        if (Array.isArray(entry) && entry.length > 0) {
          return entry[0].startsWith('b');
        }
        return false;
      })
      .map(entry => entry[0]);
  }
  
  /**
   * Set parent place
   * @param {string} parentId - Parent place ID
   * @returns {boolean} Success status
   */
  setParent(parentId) {
    if (!parentId) return false;
    
    this.par = parentId;
    return true;
  }
}

module.exports = Place;