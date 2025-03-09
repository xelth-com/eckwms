// services/cacheService.js
const NodeCache = require('node-cache');
const logger = require('../utils/logging');

/**
 * Cache Service for application-wide caching
 */
class CacheService {
  /**
   * Initialize the cache service
   * @param {Object} options - Cache options
   */
  constructor(options = {}) {
    this.cache = new NodeCache({
      stdTTL: 300, // Default cache expiration in seconds (5 minutes)
      checkperiod: 60, // Check for expired keys every 60 seconds
      useClones: false, // Store/retrieve without cloning
      ...options
    });
    
    // Set up cache statistics logging
    this.setupStatsLogging();
    
    logger.info('Cache service initialized');
  }

  /**
   * Set up periodic cache statistics logging
   */
  setupStatsLogging() {
    // Log cache stats every hour
    setInterval(() => {
      const stats = this.cache.getStats();
      logger.info('Cache statistics', { stats });
    }, 3600000); // 1 hour
  }

  /**
   * Get item from cache
   * @param {string} key - Cache key
   * @returns {*} Cached value or undefined if not found
   */
  get(key) {
    return this.cache.get(key);
  }

  /**
   * Store item in cache
   * @param {string} key - Cache key
   * @param {*} value - Value to cache
   * @param {number} ttl - Time to live in seconds (optional)
   * @returns {boolean} True on success
   */
  set(key, value, ttl = undefined) {
    return this.cache.set(key, value, ttl);
  }

  /**
   * Remove item from cache
   * @param {string} key - Cache key
   * @returns {number} Number of removed keys (0 or 1)
   */
  delete(key) {
    return this.cache.del(key);
  }

  /**
   * Check if key exists in cache
   * @param {string} key - Cache key
   * @returns {boolean} True if key exists
   */
  has(key) {
    return this.cache.has(key);
  }

  /**
   * Get multiple items from cache
   * @param {string[]} keys - Array of cache keys
   * @returns {Object} Object with key-value pairs of found items
   */
  getMany(keys) {
    return this.cache.mget(keys);
  }

  /**
   * Store multiple items in cache
   * @param {Object} items - Object with key-value pairs to cache
   * @param {number} ttl - Time to live in seconds (optional)
   * @returns {boolean} True on success
   */
  setMany(items, ttl = undefined) {
    return this.cache.mset(
      Object.entries(items).map(([key, value]) => ({ key, val: value, ttl }))
    );
  }

  /**
   * Remove multiple items from cache
   * @param {string[]} keys - Array of cache keys to remove
   * @returns {number} Number of removed keys
   */
  deleteMany(keys) {
    return this.cache.del(keys);
  }

  /**
   * Clear entire cache
   * @returns {void}
   */
  clear() {
    this.cache.flushAll();
    logger.info('Cache cleared');
  }

  /**
   * Get or set cache value with automatic refresh
   * @param {string} key - Cache key
   * @param {Function} fetchFn - Async function to fetch data if not in cache
   * @param {number} ttl - Time to live in seconds (optional)
   * @returns {Promise<*>} Cached or fetched value
   */
  async getOrSet(key, fetchFn, ttl = undefined) {
    const value = this.get(key);
    
    if (value !== undefined) {
      return value;
    }
    
    try {
      const fetchedValue = await fetchFn();
      this.set(key, fetchedValue, ttl);
      return fetchedValue;
    } catch (error) {
      logger.error(`Error fetching data for cache key ${key}:`, error);
      throw error;
    }
  }

  /**
   * Get cache stats
   * @returns {Object} Cache statistics
   */
  getStats() {
    return this.cache.getStats();
  }

  /**
   * Create a cache middleware for Express
   * @param {number} ttl - Time to live in seconds (optional)
   * @returns {Function} Express middleware
   */
  middleware(ttl = undefined) {
    return (req, res, next) => {
      // Skip caching for non-GET requests or if cache is disabled for this request
      if (req.method !== 'GET' || req.headers['x-no-cache']) {
        return next();
      }
      
      // Create cache key from URL and auth info
      const userId = req.user?.u || 'anonymous';
      const cacheKey = `${userId}:${req.originalUrl}`;
      
      // Check if response is in cache
      const cachedResponse = this.get(cacheKey);
      
      if (cachedResponse) {
        // Return cached response
        res.set('X-Cache', 'HIT');
        res.status(cachedResponse.status).send(cachedResponse.body);
        return;
      }
      
      // Cache miss, continue to handler
      res.set('X-Cache', 'MISS');
      
      // Store original send method
      const originalSend = res.send;
      
      // Override send method to cache response
      res.send = function(body) {
        // Only cache successful responses
        if (res.statusCode >= 200 && res.statusCode < 300) {
          const cacheEntry = {
            status: res.statusCode,
            body: body,
            headers: res.getHeaders()
          };
          
          // Cache the response
          this.set(cacheKey, cacheEntry, ttl);
        }
        
        // Call original send method
        return originalSend.call(this, body);
      }.bind(this);
      
      next();
    };
  }
}

// Create and export a singleton instance
const cacheService = new CacheService();
module.exports = cacheService;