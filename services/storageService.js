// services/storageService.js
const fs = require('fs').promises;
const path = require('path');
const readline = require('readline');
const { createReadStream, createWriteStream } = require('fs');
const logger = require('../utils/logging');

/**
 * Service for managing data persistence
 */
class StorageService {
  /**
   * Initialize the storage service
   * @param {string} baseDirectory - Base directory for storage
   */
  constructor(baseDirectory) {
    this.baseDirectory = baseDirectory;
    this.dataCollections = new Map();
    this.historyCollections = new Map(); // Store for historical data
    this.initialized = false;
    this.historyRetentionDays = 90; // Keep 90 days of history in memory
    this.caseInsensitiveMap = new Map(); // Map for case-insensitive lookups
    this.autoSaveInterval = null; // Interval for automatic saving
    this.dirtyCollections = new Set(); // Collections that have changes to be saved
  }

  /**
   * Initialize all data collections from storage
   * @returns {Promise<boolean>}
   */
  async initialize() {
    try {
      // Define collections and their prototype objects
      const collections = [
        { name: 'users', file: 'users.json', prototype: require('../models/user'), history: false },
        { name: 'orders', file: 'orders.json', prototype: require('../models/order'), history: true },
        { name: 'items', file: 'items.json', prototype: require('../models/item'), history: true },
        { name: 'boxes', file: 'boxes.json', prototype: require('../models/box'), history: true },
        { name: 'places', file: 'places.json', prototype: require('../models/place'), history: false },
        { name: 'classes', file: 'classes.json', prototype: require('../models/betruger'), history: false },
        { name: 'uppers', file: 'uppers.json', prototype: require('../models/item'), history: false },
        { name: 'dicts', file: 'dicts.json', prototype: require('../models/dict'), history: false }
      ];
      
      // Create base directory if it doesn't exist
      const baseDir = path.resolve(this.baseDirectory);
      const baseDirPath = path.join(baseDir, 'base');
      await fs.mkdir(baseDirPath, { recursive: true });
      
      // Create history directories if they don't exist
      const historyDirPath = path.resolve(path.join(this.baseDirectory, 'history'));
      await fs.mkdir(historyDirPath, { recursive: true });
      
      // Initialize collections
      for (const collection of collections) {
        this.dataCollections.set(collection.name, new Map());
        
        // Initialize history collection if needed
        if (collection.history) {
          this.historyCollections.set(collection.name, new Map());
          // Create history subdirectory for this collection
          await fs.mkdir(path.join(historyDirPath, collection.name), { recursive: true });
        }
        
        try {
          await this.loadCollection(collection.name, collection.file, collection.prototype);
        } catch (error) {
          logger.error(`Error loading collection ${collection.name}: ${error.message}`);
          // Continue with other collections even if one fails
        }
        
        // Load history for collections that need it
        if (collection.history) {
          try {
            await this.loadHistoryCollection(collection.name, collection.prototype);
          } catch (error) {
            logger.error(`Error loading history for ${collection.name}: ${error.message}`);
            // Continue with other collections even if history loading fails
          }
        }
      }
      
      // Load serial numbers
      try {
        await this.loadSerialNumbers();
      } catch (error) {
        logger.error(`Error loading serial numbers: ${error.message}`);
        // Initialize default serial numbers if loading fails
        this.serialIi = 999999999999999;
        this.serialI = 1;
        this.serialB = 1;
        this.serialP = 1;
      }
      
      // Set up auto-save interval
      this.autoSaveInterval = setInterval(() => {
        this.saveAll().catch(error => {
          logger.error(`Auto-save failed: ${error.message}`);
        });
      }, 5 * 60 * 1000); // 5 minutes
      
      this.initialized = true;
      logger.info('Storage service initialized successfully');
      return true;
    } catch (error) {
      logger.error(`Failed to initialize storage: ${error.message}`, error);
      return false;
    }
  }
  
  /**
   * Load a single collection from storage
   * @param {string} collectionName - Name of the collection
   * @param {string} fileName - File name to load from
   * @param {Object} prototype - Prototype object for items in this collection
   * @returns {Promise<void>}
   */
  async loadCollection(collectionName, fileName, prototype) {
    try {
      const filePath = path.resolve(path.join(this.baseDirectory, 'base', fileName));
      const collection = this.dataCollections.get(collectionName);
      
      try {
        // Check if file exists and is readable
        await fs.access(filePath, fs.constants.R_OK);
        
        // Get file stats to check if it's empty
        const stats = await fs.stat(filePath);
        if (stats.size === 0) {
          logger.warn(`File ${fileName} exists but is empty`);
          return;
        }
        
        // Create readline interface for streaming file
        const readInterface = readline.createInterface({
          input: createReadStream(filePath),
          crlfDelay: Infinity
        });
        
        // Process each line
        for await (const line of readInterface) {
          try {
            if (!line.trim()) continue; // Skip empty lines
            
            const jsonObj = JSON.parse(line);
            
            // Handle different types of objects
            if (jsonObj && jsonObj.sn && Array.isArray(jsonObj.sn) && jsonObj.sn.length > 0) {
              const key = jsonObj.sn[0];
              if (key !== undefined) {
                if (jsonObj.cl && this.dataCollections.has('classes') && 
                    this.dataCollections.get('classes').has(jsonObj.cl)) {
                  Object.setPrototypeOf(jsonObj, this.dataCollections.get('classes').get(jsonObj.cl));
                } else {
                  Object.setPrototypeOf(jsonObj, prototype);
                }
                
                // Store in main collection
                collection.set(key, jsonObj);
                
                // Add to case-insensitive map for item lookups
                if (collectionName === 'items' || collectionName === 'boxes') {
                  const lowerKey = key.toLowerCase();
                  this.caseInsensitiveMap.set(lowerKey, key);
                }
              }
            } else if (jsonObj && jsonObj.cl) {
              const key = jsonObj.cl;
              Object.setPrototypeOf(jsonObj, prototype);
              collection.set(key, jsonObj);
            } else if (jsonObj && jsonObj.orig) {
              const key = jsonObj.orig;
              Object.setPrototypeOf(jsonObj, prototype);
              collection.set(key, jsonObj);
            }
          } catch (err) {
            logger.error(`Error processing line in ${fileName}: ${err.message}`);
          }
        }
        
        logger.info(`Loaded ${collection.size} items into ${collectionName}`);
      } catch (err) {
        // If the file doesn't exist, just log a warning
        if (err.code === 'ENOENT') {
          logger.warn(`File ${fileName} does not exist - will be created when items are saved`);
        } else {
          logger.error(`Error accessing file ${fileName}: ${err.message}`);
          throw err;
        }
      }
    } catch (error) {
      logger.error(`Error loading collection ${collectionName}: ${error.message}`);
      throw error;
    }
  }
  
  /**
   * Load historical data for a collection
   * @param {string} collectionName - Name of the collection
   * @param {Object} prototype - Prototype object for items in this collection
   * @returns {Promise<void>}
   */
  async loadHistoryCollection(collectionName, prototype) {
    try {
      const historyDir = path.resolve(path.join(this.baseDirectory, 'history', collectionName));
      const historyCollection = this.historyCollections.get(collectionName);
      
      // Create history directory if it doesn't exist
      await fs.mkdir(historyDir, { recursive: true });
      
      // Get current date for retention calculation
      const now = new Date();
      const retentionThreshold = new Date(now);
      retentionThreshold.setDate(retentionThreshold.getDate() - this.historyRetentionDays);
      
      // Get list of history files
      let files;
      try {
        files = await fs.readdir(historyDir);
      } catch (err) {
        logger.warn(`History directory for ${collectionName} is empty or doesn't exist`);
        files = [];
      }
      
      // Filter files based on retention period
      const recentFiles = files.filter(file => {
        // Extract date from filename (format: YYYY-MM-DD.json)
        const dateMatch = file.match(/^(\d{4}-\d{2}-\d{2})\.json$/);
        if (!dateMatch) return false;
        
        const fileDate = new Date(dateMatch[1]);
        return fileDate >= retentionThreshold;
      });
      
      // Load each recent history file
      for (const file of recentFiles) {
        const filePath = path.join(historyDir, file);
        
        try {
          // Check if file exists and is readable
          await fs.access(filePath, fs.constants.R_OK);
          
          // Get file stats to check if it's empty
          const stats = await fs.stat(filePath);
          if (stats.size === 0) {
            continue; // Skip empty files
          }
          
          // Create readline interface for streaming file
          const readInterface = readline.createInterface({
            input: createReadStream(filePath),
            crlfDelay: Infinity
          });
          
          // Process each line
          for await (const line of readInterface) {
            try {
              if (!line.trim()) continue; // Skip empty lines
              
              const historyEntry = JSON.parse(line);
              
              // Ensure data field exists
              if (!historyEntry.data) {
                historyEntry.data = {};
              }
              
              // Set prototype
              Object.setPrototypeOf(historyEntry.data, prototype);
              
              // Store in history collection using combined key (id + timestamp)
              const historyKey = `${historyEntry.id}_${historyEntry.timestamp}`;
              historyCollection.set(historyKey, historyEntry);
            } catch (err) {
              logger.error(`Error processing history line in ${file}: ${err.message}`);
            }
          }
        } catch (err) {
          if (err.code === 'ENOENT') {
            logger.warn(`History file doesn't exist: ${file}`);
          } else {
            logger.error(`Error reading history file ${file}: ${err.message}`);
          }
        }
      }
      
      logger.info(`Loaded ${historyCollection.size} history entries for ${collectionName}`);
    } catch (error) {
      logger.error(`Error loading history for collection ${collectionName}: ${error.message}`);
      throw error;
    }
  }
  
  /**
   * Load serial number counters
   * @returns {Promise<void>}
   */
  async loadSerialNumbers() {
    try {
      const filePath = path.resolve(path.join(this.baseDirectory, 'base', 'ini.json'));
      
      try {
        // Check if file exists and is readable
        await fs.access(filePath, fs.constants.R_OK);
        
        const data = await fs.readFile(filePath, 'utf8');
        const { serialIi, serialI, serialB, serialP } = JSON.parse(data);
        
        this.serialIi = serialIi || 999999999999999;
        this.serialI = serialI || 1;
        this.serialB = serialB || 1;
        this.serialP = serialP || 1;
        
        logger.info('Serial numbers loaded successfully');
      } catch (err) {
        // Initialize default serial numbers if file doesn't exist or is invalid
        if (err.code === 'ENOENT') {
          logger.warn(`Serial numbers file does not exist - creating with defaults`);
          
          // Initialize default serial numbers
          this.serialIi = 999999999999999;
          this.serialI = 1;
          this.serialB = 1;
          this.serialP = 1;
          
          // Save the default values to create the file
          await fs.writeFile(
            filePath,
            JSON.stringify({
              serialIi: this.serialIi,
              serialI: this.serialI,
              serialB: this.serialB,
              serialP: this.serialP
            }, null, 2)
          );
        } else {
          logger.error(`Error reading serial numbers: ${err.message}`);
          throw err;
        }
      }
    } catch (error) {
      logger.error(`Error loading serial numbers: ${error.message}`);
      throw error;
    }
  }
  
  /**
   * Get a collection by name
   * @param {string} collectionName - Name of the collection
   * @returns {Map|null} The collection or null if not found
   */
  getCollection(collectionName) {
    if (!this.initialized) {
      logger.error('Storage service not initialized');
      return null;
    }
    
    if (!this.dataCollections.has(collectionName)) {
      logger.error(`Collection not found: ${collectionName}`);
      return null;
    }
    
    return this.dataCollections.get(collectionName);
  }
  
  /**
   * Get an item from a collection
   * @param {string} collectionName - Name of the collection
   * @param {string} id - Item ID
   * @returns {Object|null} The item or null if not found
   */
  getItem(collectionName, id) {
    if (!this.initialized) {
      logger.error('Storage service not initialized');
      return null;
    }
    
    if (!this.dataCollections.has(collectionName)) {
      logger.error(`Collection not found: ${collectionName}`);
      return null;
    }
    
    const collection = this.dataCollections.get(collectionName);
    
    // Try direct lookup first
    if (collection.has(id)) {
      return collection.get(id);
    }
    
    // Try case-insensitive lookup for items and boxes
    if ((collectionName === 'items' || collectionName === 'boxes') && id) {
      const lowerCaseId = id.toLowerCase();
      const canonicalId = this.caseInsensitiveMap.get(lowerCaseId);
      
      if (canonicalId && collection.has(canonicalId)) {
        return collection.get(canonicalId);
      }
    }
    
    return null;
  }
  
  /**
   * Save an item to a collection
   * @param {string} collectionName - Name of the collection
   * @param {string} id - Item ID
   * @param {Object} item - Item to save
   * @returns {boolean} Success status
   */
  saveItem(collectionName, id, item) {
    if (!this.initialized) {
      logger.error('Storage service not initialized');
      return false;
    }
    
    if (!this.dataCollections.has(collectionName)) {
      logger.error(`Collection not found: ${collectionName}`);
      return false;
    }
    
    if (!id) {
      logger.error('Cannot save item with null or undefined ID');
      return false;
    }
    
    const collection = this.dataCollections.get(collectionName);
    
    // Add or update the item
    collection.set(id, item);
    
    // Update case-insensitive map for items and boxes
    if (collectionName === 'items' || collectionName === 'boxes') {
      const lowerCaseId = id.toLowerCase();
      this.caseInsensitiveMap.set(lowerCaseId, id);
    }
    
    // Mark collection as dirty for auto-save
    this.dirtyCollections.add(collectionName);
    
    return true;
  }
  
  /**
   * Delete an item from a collection
   * @param {string} collectionName - Name of the collection
   * @param {string} id - Item ID
   * @returns {boolean} Success status
   */
  deleteItem(collectionName, id) {
    if (!this.initialized) {
      logger.error('Storage service not initialized');
      return false;
    }
    
    if (!this.dataCollections.has(collectionName)) {
      logger.error(`Collection not found: ${collectionName}`);
      return false;
    }
    
    const collection = this.dataCollections.get(collectionName);
    
    // Try direct lookup first
    if (collection.has(id)) {
      collection.delete(id);
      
      // Mark collection as dirty for auto-save
      this.dirtyCollections.add(collectionName);
      
      // Remove from case-insensitive map if needed
      if (collectionName === 'items' || collectionName === 'boxes') {
        const lowerCaseId = id.toLowerCase();
        this.caseInsensitiveMap.delete(lowerCaseId);
      }
      
      return true;
    }
    
    // Try case-insensitive lookup for items and boxes
    if ((collectionName === 'items' || collectionName === 'boxes') && id) {
      const lowerCaseId = id.toLowerCase();
      const canonicalId = this.caseInsensitiveMap.get(lowerCaseId);
      
      if (canonicalId && collection.has(canonicalId)) {
        collection.delete(canonicalId);
        this.caseInsensitiveMap.delete(lowerCaseId);
        
        // Mark collection as dirty for auto-save
        this.dirtyCollections.add(collectionName);
        
        return true;
      }
    }
    
    return false;
  }
  
  /**
   * Generate a new serial number
   * @param {string} prefix - Serial number prefix ('i' for item, 'b' for box, 'p' for place)
   * @returns {string} Generated serial number
   */
  generateSerialNumber(prefix) {
    if (!this.initialized) {
      logger.error('Storage service not initialized');
      return null;
    }
    
    let counter;
    
    // Select the appropriate counter based on prefix
    switch (prefix) {
      case 'i':
        counter = ++this.serialI;
        break;
      case 'b':
        counter = ++this.serialB;
        break;
      case 'p':
        counter = ++this.serialP;
        break;
      default:
        logger.error(`Invalid serial number prefix: ${prefix}`);
        return null;
    }
    
    // Format the serial number
    const serialNumber = `${prefix}${counter.toString().padStart(18, '0')}`;
    
    // Save updated counters
    this.saveSerialNumbers();
    
    return serialNumber;
  }
  
  /**
   * Save serial number counters
   * @returns {Promise<boolean>} Success status
   */
  async saveSerialNumbers() {
    try {
      const filePath = path.resolve(path.join(this.baseDirectory, 'base', 'ini.json'));
      
      // Prepare data
      const data = {
        serialIi: this.serialIi,
        serialI: this.serialI,
        serialB: this.serialB,
        serialP: this.serialP
      };
      
      // Write to file
      await fs.writeFile(filePath, JSON.stringify(data, null, 2));
      
      logger.debug('Serial numbers saved successfully');
      return true;
    } catch (error) {
      logger.error(`Failed to save serial numbers: ${error.message}`);
      return false;
    }
  }
  
  /**
   * Save all dirty collections to disk
   * @returns {Promise<boolean>} Success status
   */
  async saveAll() {
    if (!this.initialized) {
      logger.error('Storage service not initialized');
      return false;
    }
    
    if (this.dirtyCollections.size === 0) {
      logger.debug('No dirty collections to save');
      return true;
    }
    
    logger.info(`Saving ${this.dirtyCollections.size} dirty collections`);
    
    try {
      // Process each dirty collection
      for (const collectionName of this.dirtyCollections) {
        try {
          await this.saveCollection(collectionName);
        } catch (error) {
          logger.error(`Failed to save collection ${collectionName}: ${error.message}`);
          // Continue with other collections even if one fails
        }
      }
      
      // Clear dirty collections set
      this.dirtyCollections.clear();
      
      logger.info('All collections saved successfully');
      return true;
    } catch (error) {
      logger.error(`Error saving collections: ${error.message}`);
      return false;
    }
  }
  
  /**
   * Save a specific collection to disk
   * @param {string} collectionName - Name of the collection
   * @returns {Promise<boolean>} Success status
   */
  async saveCollection(collectionName) {
    try {
      const collection = this.dataCollections.get(collectionName);
      
      if (!collection) {
        logger.error(`Collection not found: ${collectionName}`);
        return false;
      }
      
      // Determine file name
      const fileName = `${collectionName}.json`;
      const filePath = path.resolve(path.join(this.baseDirectory, 'base', fileName));
      
      // Create temp file path
      const tempFilePath = `${filePath}.tmp`;
      
      // Create write stream for temp file
      const writeStream = createWriteStream(tempFilePath);
      
      // Write each item to the file
      for (const item of collection.values()) {
        const jsonString = JSON.stringify(item) + '\n';
        writeStream.write(jsonString);
      }
      
      // Close the write stream
      await new Promise((resolve, reject) => {
        writeStream.end(err => {
          if (err) reject(err);
          else resolve();
        });
      });
      
      // Rename temp file to actual file (atomic operation)
      await fs.rename(tempFilePath, filePath);
      
      logger.info(`Saved ${collection.size} items to ${fileName}`);
      return true;
    } catch (error) {
      logger.error(`Failed to save collection ${collectionName}: ${error.message}`);
      return false;
    }
  }
  
  /**
   * Update item location
   * @param {string} itemId - Item ID
   * @param {string} locationId - Location ID
   * @returns {boolean} Success status
   */
  updateItemLocation(itemId, locationId) {
    if (!this.initialized) {
      logger.error('Storage service not initialized');
      return false;
    }
    
    // Get the item
    const item = this.getItem('items', itemId);
    
    if (!item) {
      logger.error(`Item not found: ${itemId}`);
      return false;
    }
    
    // Update location
    if (typeof item.setLocation === 'function') {
      item.setLocation(locationId);
    } else {
      // Fallback if method doesn't exist
      if (!item.loc || !Array.isArray(item.loc)) {
        item.loc = [];
      }
      
      const timestamp = Math.floor(Date.now() / 1000);
      item.loc.push([locationId, timestamp]);
      
      // Limit history size
      if (item.loc.length > 10) {
        item.loc = item.loc.slice(-10);
      }
    }
    
    // Save the item
    return this.saveItem('items', itemId, item);
  }
  
  /**
   * Cleanup old history data
   * @returns {Promise<boolean>} Success status
   */
  async cleanupHistory() {
    if (!this.initialized) {
      logger.error('Storage service not initialized');
      return false;
    }
    
    try {
      // Get collections with history
      const historyCollections = Array.from(this.historyCollections.keys());
      
      for (const collectionName of historyCollections) {
        try {
          // Get the history collection
          const historyCollection = this.historyCollections.get(collectionName);
          
          // Get current date for retention calculation
          const now = new Date();
          const cutoffTime = Math.floor(now.getTime() / 1000) - (this.historyRetentionDays * 24 * 60 * 60);
          
          // Find old entries to delete
          const oldEntries = [];
          
          for (const [key, entry] of historyCollection.entries()) {
            if (entry.timestamp < cutoffTime) {
              oldEntries.push(key);
            }
          }
          
          // Delete old entries
          for (const key of oldEntries) {
            historyCollection.delete(key);
          }
          
          logger.info(`Removed ${oldEntries.length} old history entries for ${collectionName}`);
        } catch (error) {
          logger.error(`Error cleaning up history for ${collectionName}: ${error.message}`);
          // Continue with other collections even if one fails
        }
      }
      
      logger.info('History cleanup completed successfully');
      return true;
    } catch (error) {
      logger.error(`History cleanup failed: ${error.message}`);
      return false;
    }
  }
  
  /**
   * Close the storage service
   * @returns {Promise<boolean>} Success status
   */
  async close() {
    if (!this.initialized) {
      logger.warn('Storage service not initialized, nothing to close');
      return true;
    }
    
    try {
      // Stop auto-save interval
      if (this.autoSaveInterval) {
        clearInterval(this.autoSaveInterval);
        this.autoSaveInterval = null;
      }
      
      // Save all dirty collections
      await this.saveAll();
      
      // Save serial numbers
      await this.saveSerialNumbers();
      
      this.initialized = false;
      logger.info('Storage service closed successfully');
      return true;
    } catch (error) {
      logger.error(`Error closing storage service: ${error.message}`);
      return false;
    }
  }
}

module.exports = StorageService;