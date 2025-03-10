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
  
  // Keep the rest of the StorageService implementation as is...
  // [The rest of the StorageService.js file would continue here]
}

module.exports = StorageService;