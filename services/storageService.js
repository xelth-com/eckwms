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
  constructor(baseDirectory) {
    this.baseDirectory = baseDirectory;
    this.dataCollections = new Map();
    this.historyCollections = new Map(); // Store for historical data
    this.initialized = false;
    this.historyRetentionDays = 90; // Keep 90 days of history in memory
    this.caseInsensitiveMap = new Map(); // Map for case-insensitive lookups
  }

  /**
   * Initialize all data collections from storage
   * @returns {Promise<void>}
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
      
      // Create history directories if they don't exist
      await fs.mkdir(path.resolve(`${this.baseDirectory}history`), { recursive: true });
      
      // Initialize collections
      for (const collection of collections) {
        this.dataCollections.set(collection.name, new Map());
        
        // Initialize history collection if needed
        if (collection.history) {
          this.historyCollections.set(collection.name, new Map());
        }
        
        await this.loadCollection(collection.name, collection.file, collection.prototype);
        
        // Load history for collections that need it
        if (collection.history) {
          await this.loadHistoryCollection(collection.name, collection.prototype);
        }
      }
      
      // Load serial numbers
      await this.loadSerialNumbers();
      
      this.initialized = true;
      logger.info('Storage service initialized successfully');
    } catch (error) {
      logger.error(`Failed to initialize storage: ${error.message}`);
      throw error;
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
      const filePath = path.resolve(`${this.baseDirectory}base/${fileName}`);
      const collection = this.dataCollections.get(collectionName);
      
      try {
        // Check if file exists
        await fs.access(filePath);
        
        // Create readline interface for streaming file
        const readInterface = readline.createInterface({
          input: createReadStream(filePath),
          crlfDelay: Infinity
        });
        
        // Process each line
        for await (const line of readInterface) {
          try {
            const jsonObj = JSON.parse(line);
            
            // Handle different types of objects
            if (Object.hasOwn(jsonObj, 'sn')) {
              const key = jsonObj.sn[0];
              if (key !== undefined) {
                if (jsonObj.cl && this.dataCollections.get('classes').has(jsonObj.cl)) {
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
            } else if (Object.hasOwn(jsonObj, 'cl')) {
              const key = jsonObj.cl;
              Object.setPrototypeOf(jsonObj, prototype);
              collection.set(key, jsonObj);
            } else if (Object.hasOwn(jsonObj, 'orig')) {
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
        logger.warn(`File ${fileName} does not exist or cannot be accessed`);
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
      const historyDir = path.resolve(`${this.baseDirectory}history/${collectionName}`);
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
          // Create readline interface for streaming file
          const readInterface = readline.createInterface({
            input: createReadStream(filePath),
            crlfDelay: Infinity
          });
          
          // Process each line
          for await (const line of readInterface) {
            try {
              const historyEntry = JSON.parse(line);
              
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
          logger.error(`Error reading history file ${file}: ${err.message}`);
        }
      }
      
      logger.info(`Loaded ${historyCollection.size} history entries for ${collectionName}`);
    } catch (error) {
      logger.error(`Error loading history for collection ${collectionName}: ${error.message}`);
    }
  }
  
  /**
   * Load serial number counters
   * @returns {Promise<void>}
   */
  async loadSerialNumbers() {
    try {
      const filePath = path.resolve(`${this.baseDirectory}base/ini.json`);
      
      try {
        const data = await fs.readFile(filePath, 'utf8');
        const { serialIi, serialI, serialB, serialP } = JSON.parse(data);
        
        this.serialIi = serialIi;
        this.serialI = serialI;
        this.serialB = serialB;
        this.serialP = serialP;
        
        logger.info('Serial numbers loaded successfully');
      } catch (err) {
        // Initialize default serial numbers if file doesn't exist
        this.serialIi = 999999999999999;
        this.serialI = 1;
        this.serialB = 1;
        this.serialP = 1;
        
        logger.warn('Using default serial numbers');
      }
    } catch (error) {
      logger.error(`Error loading serial numbers: ${error.message}`);
      throw error;
    }
  }
  
  /**
   * Save all collections to storage
   * @returns {Promise<void>}
   */
  async saveAll() {
    try {
      const savePromises = [];
      
      // Save each collection
      for (const [name, collection] of this.dataCollections.entries()) {
        savePromises.push(this.saveCollection(name, `${name}.json`, collection));
      }
      
      // Save history collections
      const today = new Date().toISOString().split('T')[0]; // YYYY-MM-DD
      for (const [name, historyCollection] of this.historyCollections.entries()) {
        if (historyCollection.size > 0) {
          savePromises.push(this.saveHistoryCollection(name, `${today}.json`, historyCollection));
        }
      }
      
      // Save serial numbers
      savePromises.push(fs.writeFile(
        path.resolve(`${this.baseDirectory}base/ini.json`), 
        JSON.stringify({
          serialIi: this.serialIi,
          serialI: this.serialI,
          serialB: this.serialB,
          serialP: this.serialP
        })
      ));
      
      await Promise.all(savePromises);
      logger.info('All data saved successfully');
    } catch (error) {
      logger.error(`Failed to save data: ${error.message}`);
      throw error;
    }
  }
  
  /**
   * Save a single collection to storage
   * @param {string} collectionName - Name of the collection
   * @param {string} fileName - File name to save to
   * @param {Map} collection - The collection to save
   * @returns {Promise<void>}
   */
  async saveCollection(collectionName, fileName, collection) {
    return new Promise((resolve, reject) => {
      try {
        const filePath = path.resolve(`${this.baseDirectory}base/${fileName}`);
        const writeStream = fs.createWriteStream(filePath);
        
        let firstEntry = true;
        
        // Write each entry to the file
        for (const [_, value] of collection) {
          if (!firstEntry) {
            writeStream.write('\n');
          } else {
            firstEntry = false;
          }
          
          writeStream.write(JSON.stringify(value));
        }
        
        writeStream.end();
        
        writeStream.on('finish', () => {
          logger.info(`Saved ${collection.size} items from ${collectionName}`);
          resolve();
        });
        
        writeStream.on('error', (err) => {
          logger.error(`Error writing ${fileName}: ${err.message}`);
          reject(err);
        });
      } catch (error) {
        logger.error(`Error saving collection ${collectionName}: ${error.message}`);
        reject(error);
      }
    });
  }
  
  /**
   * Save history collection to storage
   * @param {string} collectionName - Name of the collection
   * @param {string} fileName - File name to save to
   * @param {Map} historyCollection - The history collection to save
   * @returns {Promise<void>}
   */
  async saveHistoryCollection(collectionName, fileName, historyCollection) {
    return new Promise((resolve, reject) => {
      try {
        const historyDir = path.resolve(`${this.baseDirectory}history/${collectionName}`);
        const filePath = path.join(historyDir, fileName);
        
        // Create directory if it doesn't exist
        fs.mkdir(historyDir, { recursive: true })
          .then(() => {
            const writeStream = createWriteStream(filePath, { flags: 'a' }); // Append mode
            
            let firstEntry = true;
            
            // Write each entry to the file
            for (const [_, value] of historyCollection) {
              if (!firstEntry) {
                writeStream.write('\n');
              } else {
                firstEntry = false;
              }
              
              writeStream.write(JSON.stringify(value));
            }
            
            writeStream.end();
            
            writeStream.on('finish', () => {
              logger.info(`Saved ${historyCollection.size} history entries for ${collectionName}`);
              
              // Clear in-memory history after saving
              historyCollection.clear();
              
              resolve();
            });
            
            writeStream.on('error', (err) => {
              logger.error(`Error writing history ${fileName}: ${err.message}`);
              reject(err);
            });
          })
          .catch(err => {
            logger.error(`Error creating history directory for ${collectionName}: ${err.message}`);
            reject(err);
          });
      } catch (error) {
        logger.error(`Error saving history collection ${collectionName}: ${error.message}`);
        reject(error);
      }
    });
  }
  
  /**
   * Get a collection by name
   * @param {string} collectionName - Name of the collection
   * @returns {Map|null} The requested collection or null if not found
   */
  getCollection(collectionName) {
    if (!this.initialized) {
      throw new Error('Storage service not initialized');
    }
    
    return this.dataCollections.get(collectionName) || null;
  }
  
  /**
   * Get an item from a collection
   * @param {string} collectionName - Name of the collection
   * @param {string} key - Item key
   * @returns {Object|null} The requested item or null if not found
   */
  getItem(collectionName, key) {
    const collection = this.getCollection(collectionName);
    
    if (!collection) return null;
    
    // Try direct lookup first
    const item = collection.get(key);
    if (item) return item;
    
    // If not found and collection is items or boxes, try case-insensitive lookup
    if ((collectionName === 'items' || collectionName === 'boxes') && typeof key === 'string') {
      const lowerKey = key.toLowerCase();
      const originalKey = this.caseInsensitiveMap.get(lowerKey);
      if (originalKey) {
        return collection.get(originalKey);
      }
    }
    
    return null;
  }
  
  /**
   * Get history for an item
   * @param {string} collectionName - Name of the collection
   * @param {string} itemId - Item ID
   * @param {number} limit - Maximum number of history entries to return
   * @returns {Array} Array of history entries
   */
  getItemHistory(collectionName, itemId, limit = 100) {
    const historyCollection = this.historyCollections.get(collectionName);
    if (!historyCollection) return [];
    
    // Normalize case for case-insensitive lookup
    let normalizedId = itemId;
    if (typeof itemId === 'string') {
      const lowerKey = itemId.toLowerCase();
      const originalKey = this.caseInsensitiveMap.get(lowerKey);
      if (originalKey) {
        normalizedId = originalKey;
      }
    }
    
    // Find all history entries for this item
    const historyEntries = [];
    for (const [key, entry] of historyCollection.entries()) {
      if (entry.id === normalizedId) {
        historyEntries.push(entry);
      }
    }
    
    // Sort by timestamp (newest first) and limit
    return historyEntries
      .sort((a, b) => b.timestamp - a.timestamp)
      .slice(0, limit);
  }
  
  /**
   * Save an item to a collection
   * @param {string} collectionName - Name of the collection
   * @param {string} key - Item key
   * @param {Object} item - The item to save
   * @param {boolean} trackHistory - Whether to track this change in history
   * @returns {boolean} True if successful
   */
  saveItem(collectionName, key, item, trackHistory = true) {
    try {
      const collection = this.getCollection(collectionName);
      if (!collection) {
        return false;
      }
      
      // If we have history for this collection and tracking is enabled
      if (trackHistory && this.historyCollections.has(collectionName)) {
        const historyCollection = this.historyCollections.get(collectionName);
        const timestamp = Math.floor(Date.now() / 1000);
        
        // Create a deep copy of the item for history
        const historyCopy = JSON.parse(JSON.stringify(item));
        
        // Add to history with metadata
        const historyEntry = {
          id: key,
          timestamp: timestamp,
          data: historyCopy
        };
        
        // Use a unique key for the history entry
        const historyKey = `${key}_${timestamp}`;
        historyCollection.set(historyKey, historyEntry);
      }
      
      // Update main collection
      collection.set(key, item);
      
      // Update case-insensitive map for item lookups
      if ((collectionName === 'items' || collectionName === 'boxes') && typeof key === 'string') {
        const lowerKey = key.toLowerCase();
        this.caseInsensitiveMap.set(lowerKey, key);
      }
      
      return true;
    } catch (error) {
      logger.error(`Error saving item to ${collectionName}: ${error.message}`);
      return false;
    }
  }
  
  /**
   * Create or update item reference in a container (box or place)
   * @param {string} containerType - Type of container ('boxes' or 'places')
   * @param {string} containerId - Container ID
   * @param {string} itemId - Item ID
   * @param {string} action - Action ('add' or 'remove')
   * @returns {boolean} True if successful
   */
  updateContainerContents(containerType, containerId, itemId, action) {
    try {
      // Get container
      const container = this.getItem(containerType, containerId);
      if (!container) {
        logger.error(`Container ${containerId} not found in ${containerType}`);
        return false;
      }
      
      // Normalize case for case-insensitive lookup
      let normalizedItemId = itemId;
      if (typeof itemId === 'string') {
        const lowerKey = itemId.toLowerCase();
        const originalKey = this.caseInsensitiveMap.get(lowerKey);
        if (originalKey) {
          normalizedItemId = originalKey;
        }
      }
      
      // Initialize contents array if it doesn't exist
      if (!container.cont || !Array.isArray(container.cont)) {
        container.cont = [];
      }
      
      const timestamp = Math.floor(Date.now() / 1000);
      
      if (action === 'add') {
        // Check if item is already in container (case insensitive)
        const existingIndex = container.cont.findIndex(entry => {
          if (Array.isArray(entry) && entry.length > 0) {
            return entry[0].toLowerCase() === normalizedItemId.toLowerCase();
          }
          return false;
        });
        
        if (existingIndex >= 0) {
          // Update timestamp if already exists
          container.cont[existingIndex][1] = timestamp;
        } else {
          // Add new entry
          container.cont.push([normalizedItemId, timestamp]);
        }
      } else if (action === 'remove') {
        // Find item in container (case insensitive)
        const existingIndex = container.cont.findIndex(entry => {
          if (Array.isArray(entry) && entry.length > 0) {
            return entry[0].toLowerCase() === normalizedItemId.toLowerCase();
          }
          return false;
        });
        
        if (existingIndex >= 0) {
          // Remove item from container
          container.cont.splice(existingIndex, 1);
        } else {
          logger.warn(`Item ${normalizedItemId} not found in container ${containerId}`);
          return false;
        }
      } else {
        logger.error(`Invalid action '${action}' for container contents update`);
        return false;
      }
      
      // Save the container
      return this.saveItem(containerType, containerId, container);
    } catch (error) {
      logger.error(`Error updating container contents: ${error.message}`);
      return false;
    }
  }
  
  /**
   * Update item location
   * @param {string} itemId - Item ID
   * @param {string} locationId - New location ID
   * @returns {boolean} True if successful
   */
  updateItemLocation(itemId, locationId) {
    try {
      // Get item
      const item = this.getItem('items', itemId);
      if (!item) {
        logger.error(`Item ${itemId} not found`);
        return false;
      }
      
      // Get location
      const location = this.getItem('places', locationId);
      if (!location) {
        logger.error(`Location ${locationId} not found`);
        return false;
      }
      
      const timestamp = Math.floor(Date.now() / 1000);
      
      // Initialize location history if it doesn't exist
      if (!item.loc || !Array.isArray(item.loc)) {
        item.loc = [];
      }
      
      // Add new location entry
      item.loc.push([locationId, timestamp]);
      
      // If we have too many history entries, remove older ones (keep only last 10)
      const MAX_LOCATION_HISTORY = 10;
      if (item.loc.length > MAX_LOCATION_HISTORY) {
        // Move older entries to history collection
        if (this.historyCollections.has('items')) {
          const historyCollection = this.historyCollections.get('items');
          const oldLocations = item.loc.slice(0, item.loc.length - MAX_LOCATION_HISTORY);
          
          for (const oldLoc of oldLocations) {
            const historyEntry = {
              id: itemId,
              timestamp: oldLoc[1],
              type: 'location',
              data: { location: oldLoc[0] }
            };
            
            const historyKey = `${itemId}_location_${oldLoc[1]}`;
            historyCollection.set(historyKey, historyEntry);
          }
        }
        
        // Keep only the most recent entries
        item.loc = item.loc.slice(-MAX_LOCATION_HISTORY);
      }
      
      // Update location's contents
      this.updateContainerContents('places', locationId, itemId, 'add');
      
      // Save the item
      return this.saveItem('items', itemId, item);
    } catch (error) {
      logger.error(`Error updating item location: ${error.message}`);
      return false;
    }
  }
  
  /**
   * Cleanup old history from memory (should be called periodically)
   * @returns {Promise<void>}
   */
  async cleanupHistory() {
    try {
      logger.info('Starting history cleanup');
      
      // Get current date for retention calculation
      const now = new Date();
      const retentionThreshold = new Date(now);
      retentionThreshold.setDate(retentionThreshold.getDate() - this.historyRetentionDays);
      const thresholdTimestamp = Math.floor(retentionThreshold.getTime() / 1000);
      
      // Process each history collection
      for (const [name, historyCollection] of this.historyCollections.entries()) {
        let entriesRemoved = 0;
        
        // Find entries older than retention threshold
        const oldEntryKeys = [];
        for (const [key, entry] of historyCollection.entries()) {
          if (entry.timestamp < thresholdTimestamp) {
            oldEntryKeys.push(key);
            entriesRemoved++;
          }
        }
        
        // Remove old entries
        for (const key of oldEntryKeys) {
          historyCollection.delete(key);
        }
        
        logger.info(`Removed ${entriesRemoved} old history entries from ${name}`);
      }
      
      logger.info('History cleanup completed');
    } catch (error) {
      logger.error(`Error during history cleanup: ${error.message}`);
    }
  }
  
  /**
   * Generate a new serial number for an entity type
   * @param {string} type - Entity type ('i', 'b', 'p')
   * @returns {string} The generated serial number
   */
  generateSerialNumber(type) {
    switch (type) {
      case 'i':
        return `i${('000000000000000000' + (++this.serialI)).slice(-18)}`;
      case 'b':
        return `b${('000000000000000000' + (++this.serialB)).slice(-18)}`;
      case 'p':
        return `p${('000000000000000000' + (++this.serialP)).slice(-18)}`;
      default:
        throw new Error(`Unknown entity type: ${type}`);
    }
  }
}

module.exports = StorageService;