// services/historyService.js
const fs = require('fs').promises;
const path = require('path');
const { createReadStream, createWriteStream } = require('fs');
const readline = require('readline');
const logger = require('../utils/logging');

/**
 * Service for managing entity history
 */
class HistoryService {
  /**
   * Initialize the history service
   * @param {string} baseDirectory - Base directory for storage
   */
  constructor(baseDirectory) {
    this.baseDirectory = baseDirectory;
    this.historyDir = path.resolve(path.join(baseDirectory, 'history'));
    this.entityHistories = new Map();
    this.retentionPeriodDays = 90; // Default retention period
    this.initialized = false;
  }
  
  /**
   * Initialize the history service
   * @returns {Promise<boolean>} Success status
   */
  async initialize() {
    try {
      // Create history directory if it doesn't exist
      await fs.mkdir(this.historyDir, { recursive: true });
      
      // Initialize entity-specific history directories
      const entityTypes = ['items', 'boxes', 'places', 'orders'];
      
      for (const entityType of entityTypes) {
        const entityDir = path.join(this.historyDir, entityType);
        await fs.mkdir(entityDir, { recursive: true });
        this.entityHistories.set(entityType, new Map());
      }
      
      this.initialized = true;
      logger.info('History service initialized successfully');
      return true;
    } catch (error) {
      logger.error(`Failed to initialize history service: ${error.message}`);
      return false;
    }
  }
  
  /**
   * Record history entry
   * @param {string} entityType - Entity type (items, boxes, places, orders)
   * @param {string} entityId - Entity ID
   * @param {string} action - Action performed
   * @param {Object} data - Data snapshot
   * @returns {Promise<boolean>} Success status
   */
  async recordHistory(entityType, entityId, action, data) {
    if (!this.initialized) {
      logger.error('History service not initialized');
      return false;
    }
    
    if (!this.entityHistories.has(entityType)) {
      logger.error(`Invalid entity type: ${entityType}`);
      return false;
    }
    
    try {
      const timestamp = Math.floor(Date.now() / 1000);
      const today = new Date().toISOString().split('T')[0]; // YYYY-MM-DD
      
      // Create history entry
      const historyEntry = {
        id: entityId,
        timestamp,
        action,
        data: JSON.parse(JSON.stringify(data)) // Deep copy
      };
      
      // Get entity history map
      const entityHistory = this.entityHistories.get(entityType);
      
      // Create a unique key for this history entry
      const historyKey = `${entityId}_${timestamp}_${action}`;
      entityHistory.set(historyKey, historyEntry);
      
      // If we accumulate too many entries, write them to disk
      const MAX_MEMORY_ENTRIES = 100;
      if (entityHistory.size > MAX_MEMORY_ENTRIES) {
        await this.flushHistoryToDisk(entityType, today);
      }
      
      return true;
    } catch (error) {
      logger.error(`Failed to record history: ${error.message}`);
      return false;
    }
  }
  
  /**
   * Flush in-memory history to disk
   * @param {string} entityType - Entity type
   * @param {string} date - Date string (YYYY-MM-DD)
   * @returns {Promise<boolean>} Success status
   */
  async flushHistoryToDisk(entityType, date) {
    try {
      const entityHistory = this.entityHistories.get(entityType);
      if (entityHistory.size === 0) {
        return true; // Nothing to flush
      }
      
      const filePath = path.join(this.historyDir, entityType, `${date}.json`);
      
      // Convert history entries to array
      const entries = Array.from(entityHistory.values());
      
      // Ensure the directory exists
      const dirPath = path.dirname(filePath);
      await fs.mkdir(dirPath, { recursive: true });
      
      // Write to file (append mode)
      let fileContent = '';
      for (const entry of entries) {
        fileContent += JSON.stringify(entry) + '\n';
      }
      
      await fs.appendFile(filePath, fileContent);
      
      // Clear in-memory history
      entityHistory.clear();
      
      logger.info(`Flushed ${entries.length} history entries for ${entityType} to disk`);
      return true;
    } catch (error) {
      logger.error(`Failed to flush history to disk: ${error.message}`);
      return false;
    }
  }
  
  /**
   * Get history for an entity
   * @param {string} entityType - Entity type
   * @param {string} entityId - Entity ID
   * @param {Object} options - Query options
   * @param {number} [options.limit=100] - Maximum number of entries
   * @param {number} [options.offset=0] - Offset to start from
   * @param {string} [options.action] - Filter by action
   * @param {number} [options.startTime] - Start timestamp
   * @param {number} [options.endTime] - End timestamp
   * @returns {Promise<Array>} History entries
   */
  async getHistory(entityType, entityId, options = {}) {
    if (!this.initialized) {
      logger.error('History service not initialized');
      return [];
    }
    
    if (!this.entityHistories.has(entityType)) {
      logger.error(`Invalid entity type: ${entityType}`);
      return [];
    }
    
    try {
      const {
        limit = 100,
        offset = 0,
        action = null,
        startTime = 0,
        endTime = Math.floor(Date.now() / 1000)
      } = options;
      
      // Get history from memory first
      const memoryHistory = this.getMemoryHistory(entityType, entityId, action, startTime, endTime);
      
      // If we have enough entries from memory, return them
      if (memoryHistory.length >= limit + offset) {
        return memoryHistory.slice(offset, offset + limit);
      }
      
      // Otherwise, we need to load from disk
      const diskHistory = await this.getDiskHistory(entityType, entityId, action, startTime, endTime);
      
      // Combine and sort
      const combinedHistory = [...memoryHistory, ...diskHistory];
      combinedHistory.sort((a, b) => b.timestamp - a.timestamp); // Newest first
      
      // Apply paging
      return combinedHistory.slice(offset, offset + limit);
    } catch (error) {
      logger.error(`Failed to get history: ${error.message}`);
      return [];
    }
  }
  
  /**
   * Get history entries from memory
   * @param {string} entityType - Entity type
   * @param {string} entityId - Entity ID
   * @param {string|null} action - Filter by action
   * @param {number} startTime - Start timestamp
   * @param {number} endTime - End timestamp
   * @returns {Array} History entries
   */
  getMemoryHistory(entityType, entityId, action, startTime, endTime) {
    const entityHistory = this.entityHistories.get(entityType);
    const results = [];
    
    for (const entry of entityHistory.values()) {
      if (entry.id !== entityId) continue;
      if (entry.timestamp < startTime || entry.timestamp > endTime) continue;
      if (action && entry.action !== action) continue;
      
      results.push(entry);
    }
    
    // Sort by timestamp (newest first)
    results.sort((a, b) => b.timestamp - a.timestamp);
    
    return results;
  }
  
  /**
   * Get history entries from disk
   * @param {string} entityType - Entity type
   * @param {string} entityId - Entity ID
   * @param {string|null} action - Filter by action
   * @param {number} startTime - Start timestamp
   * @param {number} endTime - End timestamp
   * @returns {Promise<Array>} History entries
   */
  async getDiskHistory(entityType, entityId, action, startTime, endTime) {
    try {
      const results = [];
      const entityDir = path.join(this.historyDir, entityType);
      
      // Check if directory exists
      try {
        await fs.access(entityDir);
      } catch (error) {
        logger.warn(`History directory does not exist: ${entityDir}`);
        return [];
      }
      
      // Get all history files
      const files = await fs.readdir(entityDir);
      
      // Filter files by date range
      const startDate = new Date(startTime * 1000);
      const endDate = new Date(endTime * 1000);
      
      const relevantFiles = files.filter(file => {
        // Format: YYYY-MM-DD.json
        const match = file.match(/^(\d{4}-\d{2}-\d{2})\.json$/);
        if (!match) return false;
        
        const fileDate = new Date(match[1]);
        return fileDate >= startDate && fileDate <= endDate;
      });
      
      // Process each file
      for (const file of relevantFiles) {
        const filePath = path.join(entityDir, file);
        
        try {
          const fileStats = await fs.stat(filePath);
          if (fileStats.size === 0) {
            continue; // Skip empty files
          }
          
          const rl = readline.createInterface({
            input: createReadStream(filePath),
            crlfDelay: Infinity
          });
          
          for await (const line of rl) {
            try {
              const entry = JSON.parse(line);
              
              // Apply filters
              if (entry.id !== entityId) continue;
              if (entry.timestamp < startTime || entry.timestamp > endTime) continue;
              if (action && entry.action !== action) continue;
              
              results.push(entry);
            } catch (error) {
              logger.error(`Error parsing history line in ${file}: ${error.message}`);
            }
          }
        } catch (error) {
          logger.error(`Error reading history file ${file}: ${error.message}`);
        }
      }
      
      // Sort by timestamp (newest first)
      results.sort((a, b) => b.timestamp - a.timestamp);
      
      return results;
    } catch (error) {
      logger.error(`Failed to get disk history: ${error.message}`);
      return [];
    }
  }
  
  /**
   * Cleanup old history files
   * @returns {Promise<boolean>} Success status
   */
  async cleanupOldHistory() {
    try {
      logger.info('Starting history cleanup');
      
      const now = new Date();
      const cutoffDate = new Date(now);
      cutoffDate.setDate(cutoffDate.getDate() - this.retentionPeriodDays);
      
      // Process each entity type
      for (const entityType of this.entityHistories.keys()) {
        const entityDir = path.join(this.historyDir, entityType);
        
        // Check if directory exists
        try {
          await fs.access(entityDir);
        } catch (error) {
          logger.warn(`History directory does not exist: ${entityDir}`);
          continue;
        }
        
        // Get all history files
        const files = await fs.readdir(entityDir);
        
        // Find old files to delete
        const oldFiles = files.filter(file => {
          // Format: YYYY-MM-DD.json
          const match = file.match(/^(\d{4}-\d{2}-\d{2})\.json$/);
          if (!match) return false;
          
          const fileDate = new Date(match[1]);
          return fileDate < cutoffDate;
        });
        
        // Delete old files
        for (const file of oldFiles) {
          const filePath = path.join(entityDir, file);
          await fs.unlink(filePath);
          logger.info(`Deleted old history file: ${filePath}`);
        }
        
        logger.info(`Cleaned up ${oldFiles.length} old history files for ${entityType}`);
      }
      
      logger.info('History cleanup completed successfully');
      return true;
    } catch (error) {
      logger.error(`History cleanup failed: ${error.message}`);
      return false;
    }
  }
  
  /**
   * Set retention period for history
   * @param {number} days - Number of days to retain history
   */
  setRetentionPeriod(days) {
    if (days < 1) {
      logger.warn('Invalid retention period. Setting to default (90 days)');
      this.retentionPeriodDays = 90;
    } else {
      this.retentionPeriodDays = days;
      logger.info(`Set history retention period to ${days} days`);
    }
  }
  
  /**
   * Export history for an entity
   * @param {string} entityType - Entity type
   * @param {string} entityId - Entity ID
   * @param {string} format - Export format ('json' or 'csv')
   * @param {Object} options - Query options
   * @returns {Promise<string|Buffer>} Exported data
   */
  async exportHistory(entityType, entityId, format = 'json', options = {}) {
    try {
      // Get history entries
      const history = await this.getHistory(entityType, entityId, options);
      
      if (format === 'csv') {
        // Convert to CSV
        let csv = 'Timestamp,Date,Action,Details\n';
        
        for (const entry of history) {
          const date = new Date(entry.timestamp * 1000).toISOString();
          const action = entry.action || '';
          const details = JSON.stringify(entry.data).replace(/"/g, '""');
          
          csv += `${entry.timestamp},"${date}","${action}","${details}"\n`;
        }
        
        return csv;
      } else {
        // Return as JSON
        return JSON.stringify(history, null, 2);
      }
    } catch (error) {
      logger.error(`Failed to export history: ${error.message}`);
      throw error;
    }
  }
}

module.exports = HistoryService;