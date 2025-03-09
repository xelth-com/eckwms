// models/box.js
const Betruger = require('./betruger');
const logger = require('../utils/logging');

/**
 * Box model for containers
 * @class Box
 * @extends Betruger
 */
class Box extends Betruger {
  /**
   * Create a new Box
   * @param {string} serialNumber - Box serial number
   * @param {string} [description] - Box description
   */
  constructor(serialNumber, description) {
    super();
    
    this.sn = [serialNumber, Math.floor(Date.now() / 1000)];
    this.cont = []; // Contents (items inside the box)
    this.loc = []; // Location history
    this.in = []; // Incoming history
    this.out = []; // Outgoing history
    this.desc = description ? [description] : [];
    this.brc = []; // Barcodes
    this.mas = []; // Mass measurements
    this.siz = []; // Size measurements
    this.mult = [[1]]; // Multiplier (default 1)
  }
  
  /**
   * Add an item to the box
   * @param {string} itemId - Item serial number
   * @returns {boolean} Success status
   */
  addItem(itemId) {
    if (!itemId) return false;
    
    const timestamp = Math.floor(Date.now() / 1000);
    
    // Check if item is already in the box (case insensitive)
    const normalizedItemId = itemId.toLowerCase();
    const existingIndex = this.cont.findIndex(entry => {
      if (Array.isArray(entry) && entry.length > 0) {
        return entry[0].toLowerCase() === normalizedItemId;
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
   * Remove an item from the box
   * @param {string} itemId - Item serial number
   * @returns {boolean} Success status
   */
  removeItem(itemId) {
    if (!itemId || !this.cont || !Array.isArray(this.cont)) return false;
    
    // Find item (case insensitive)
    const normalizedItemId = itemId.toLowerCase();
    const index = this.cont.findIndex(entry => {
      if (Array.isArray(entry) && entry.length > 0) {
        return entry[0].toLowerCase() === normalizedItemId;
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
   * Check if the box contains an item
   * @param {string} itemId - Item serial number
   * @returns {boolean} True if box contains the item
   */
  hasItem(itemId) {
    if (!itemId || !this.cont || !Array.isArray(this.cont)) return false;
    
    // Check for item (case insensitive)
    const normalizedItemId = itemId.toLowerCase();
    return this.cont.some(entry => {
      if (Array.isArray(entry) && entry.length > 0) {
        return entry[0].toLowerCase() === normalizedItemId;
      }
      return false;
    });
  }
  
  /**
   * Set box location
   * @param {string} locationId - Location ID
   * @returns {boolean} Success status
   */
  setLocation(locationId) {
    if (!locationId) return false;
    
    const timestamp = Math.floor(Date.now() / 1000);
    
    // Initialize location array if it doesn't exist
    if (!this.loc || !Array.isArray(this.loc)) {
      this.loc = [];
    }
    
    // Add new location
    this.loc.push([locationId, timestamp]);
    
    // Limit history size (keep only last 10 entries)
    if (this.loc.length > 10) {
      this.loc = this.loc.slice(-10);
    }
    
    return true;
  }
  
  /**
   * Get current location ID
   * @returns {string|null} Location ID or null if no location set
   */
  getCurrentLocation() {
    if (!this.loc || !Array.isArray(this.loc) || this.loc.length === 0) {
      return null;
    }
    
    // Return the most recent location ID
    const lastLoc = this.loc[this.loc.length - 1];
    if (Array.isArray(lastLoc) && lastLoc.length > 0) {
      return lastLoc[0];
    }
    
    return null;
  }
  
  /**
   * Add barcode to the box
   * @param {string} barcode - Barcode value
   * @returns {boolean} Success status
   */
  addBarcode(barcode) {
    if (!barcode) return false;
    
    // Initialize barcodes array if it doesn't exist
    if (!this.brc || !Array.isArray(this.brc)) {
      this.brc = [];
    }
    
    // Check if barcode already exists
    if (this.brc.includes(barcode)) {
      return true;
    }
    
    // Add barcode
    this.brc.push(barcode);
    return true;
  }
  
  /**
   * Get all items in the box
   * @returns {Array} Array of item IDs
   */
  getItems() {
    if (!this.cont || !Array.isArray(this.cont)) {
      return [];
    }
    
    // Extract item IDs from contents
    return this.cont.map(entry => {
      if (Array.isArray(entry) && entry.length > 0) {
        return entry[0];
      }
      return null;
    }).filter(id => id !== null);
  }
  
  /**
   * Record incoming transfer
   * @param {string} sourceId - Source ID
   * @returns {boolean} Success status
   */
  recordIncoming(sourceId) {
    if (!sourceId) return false;
    
    const timestamp = Math.floor(Date.now() / 1000);
    
    // Initialize incoming array if it doesn't exist
    if (!this.in || !Array.isArray(this.in)) {
      this.in = [];
    }
    
    // Add incoming record
    this.in.push([sourceId, timestamp]);
    return true;
  }
  
  /**
   * Record outgoing transfer
   * @param {string} destinationId - Destination ID
   * @returns {boolean} Success status
   */
  recordOutgoing(destinationId) {
    if (!destinationId) return false;
    
    const timestamp = Math.floor(Date.now() / 1000);
    
    // Initialize outgoing array if it doesn't exist
    if (!this.out || !Array.isArray(this.out)) {
      this.out = [];
    }
    
    // Add outgoing record
    this.out.push([destinationId, timestamp]);
    return true;
  }
  
  /**
   * Set box multiplier
   * @param {number} value - Multiplier value
   * @returns {boolean} Success status
   */
  setMultiplier(value) {
    const multiplier = parseInt(value);
    if (isNaN(multiplier) || multiplier < 1) return false;
    
    // Initialize multiplier array if it doesn't exist
    if (!this.mult || !Array.isArray(this.mult)) {
      this.mult = [];
    }
    
    this.mult = [[multiplier]];
    return true;
  }
  
  /**
   * Get box multiplier
   * @returns {number} Multiplier value
   */
  getMultiplier() {
    if (!this.mult || !Array.isArray(this.mult) || this.mult.length === 0) {
      return 1;
    }
    
    const entry = this.mult[0];
    if (Array.isArray(entry) && entry.length > 0) {
      const value = parseInt(entry[0]);
      return isNaN(value) ? 1 : value;
    }
    
    return 1;
  }
}

module.exports = Box;