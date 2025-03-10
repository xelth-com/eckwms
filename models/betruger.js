// models/betruger.js
/**
 * Base model class for WMS entities
 * @class Betruger
 */
class Betruger {
    /**
     * Create a new base object
     */
    constructor() {
      // Default initialization (all specialized properties are added by child classes)
    }
    
    /**
     * Convert object to JSON string
     * @returns {string} JSON representation
     */
    toJSON() {
      return JSON.stringify(this);
    }
    
    /**
     * Convert object to string representation
     * @returns {string} String representation
     */
    toString() {
      if (this.sn && Array.isArray(this.sn) && this.sn.length > 0) {
        return this.sn[0];
      }
      
      if (this.cl) {
        return this.cl;
      }
      
      return '[Object Betruger]';
    }
    
    /**
     * Get a property value
     * @param {string} prop - Property name
     * @returns {*} Property value
     */
    get(prop) {
      return this[prop];
    }
    
    /**
     * Set a property value
     * @param {string} prop - Property name
     * @param {*} value - Property value
     */
    set(prop, value) {
      this[prop] = value;
    }
  }
  
  module.exports = Betruger;