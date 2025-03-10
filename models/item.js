// models/item.js
const Betruger = require('./betruger');
const logger = require('../utils/logging');

/**
 * Item model for devices and parts
 * @class Item
 * @extends Betruger
 */
class Item extends Betruger {
  /**
   * Create a new Item
   * @param {string} serialNumber - Item serial number
   * @param {string} [className] - Class name
   * @param {string} [description] - Item description
   */
  constructor(serialNumber, className, description) {
    super();
    
    this.sn = [serialNumber, Math.floor(Date.now() / 1000)];
    this.cl = className || null;
    this.desc = description ? [description] : [];
    this.cond = []; // Condition notes
    this.actn = []; // Actions performed on the item
    this.img = []; // Images
    this.mas = []; // Mass measurements
    this.siz = []; // Size measurements
    this.own = []; // Ownership information
    this.loc = []; // Location history
    this.brc = []; // Barcodes
    this.attr = {}; // Additional attributes
  }
  
  /**
   * Add an action to the item's history
   * @param {string} type - Action type (check, repair, note)
   * @param {string} message - Action message
   * @returns {boolean} Success status
   */
  addAction(type, message) {
    if (!type || !message) return false;
    
    const timestamp = Math.floor(Date.now() / 1000);
    
    // Initialize actions array if it doesn't exist
    if (!this.actn || !Array.isArray(this.actn)) {
      this.actn = [];
    }
    
    // Add action
    this.actn.push([type, message, timestamp]);
    
    // Limit history size (keep only last 20 actions)
    if (this.actn.length > 20) {
      // Remove oldest actions
      this.actn = this.actn.slice(-20);
    }
    
    return true;
  }
  
  /**
   * Set item location
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
   * Add barcode to the item
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
   * Set item condition
   * @param {string} condition - Condition description
   * @returns {boolean} Success status
   */
  setCondition(condition) {
    if (!condition) return false;
    
    // Initialize condition array if it doesn't exist
    if (!this.cond || !Array.isArray(this.cond)) {
      this.cond = [];
    }
    
    // Add condition (replace previous if exists)
    if (this.cond.length > 0) {
      this.cond[0] = condition;
    } else {
      this.cond.push(condition);
    }
    
    return true;
  }
  
  /**
   * Set item owner
   * @param {string} owner - Owner ID or name
   * @returns {boolean} Success status
   */
  setOwner(owner) {
    if (!owner) return false;
    
    const timestamp = Math.floor(Date.now() / 1000);
    
    // Initialize ownership array if it doesn't exist
    if (!this.own || !Array.isArray(this.own)) {
      this.own = [];
    }
    
    // Add ownership record
    this.own.push([owner, timestamp]);
    return true;
  }
  
  /**
   * Get current owner
   * @returns {string|null} Owner ID or null if no owner set
   */
  getCurrentOwner() {
    if (!this.own || !Array.isArray(this.own) || this.own.length === 0) {
      return null;
    }
    
    // Return the most recent owner
    const lastOwner = this.own[this.own.length - 1];
    if (Array.isArray(lastOwner) && lastOwner.length > 0) {
      return lastOwner[0];
    }
    
    return null;
  }
  
  /**
   * Set item attribute
   * @param {string} name - Attribute name
   * @param {*} value - Attribute value
   * @returns {boolean} Success status
   */
  setAttribute(name, value) {
    if (!name) return false;
    
    // Initialize attributes object if it doesn't exist
    if (!this.attr || typeof this.attr !== 'object') {
      this.attr = {};
    }
    
    // Set attribute
    this.attr[name] = value;
    return true;
  }
  
  /**
   * Get item attribute
   * @param {string} name - Attribute name
   * @returns {*} Attribute value or undefined if not found
   */
  getAttribute(name) {
    if (!name || !this.attr || typeof this.attr !== 'object') {
      return undefined;
    }
    
    return this.attr[name];
  }
}

module.exports = Item;