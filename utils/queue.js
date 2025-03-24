// utils/queue.js - Optimized queue implementation for translation processing

/**
 * Enhanced priority-based queue implementation for translation processing
 * Includes priority scheduling, duplicate prevention, and retry management
 */
class Queue {
  /**
   * Create a new priority-based queue
   */
  constructor() {
    this.items = [];                  // Actual queue items
    this.processingMap = new Map();   // Track items currently being processed
    this.processedKeys = new Set();   // Track keys that have been processed to avoid duplicates
    this.priorityScores = new Map();  // Store priority scores for language/namespace combinations
    this.stats = {                    // Statistics for monitoring
      enqueued: 0,
      dequeued: 0,
      completed: 0,
      failed: 0,
      retried: 0
    };
  }

  /**
   * Add an item to the queue with priority-based positioning
   * @param {Object} item - Item to add to the queue (must include targetLang, namespace, and key properties)
   * @return {boolean} - Whether the item was successfully added
   */
  enqueue(item) {
    // Generate a unique key for deduplication
    const uniqueKey = `${item.targetLang}:${item.namespace || 'common'}:${item.key || this._generateKeyFromText(item.text)}`;
    
    // Skip if this item is currently being processed
    if (this.processingMap.has(uniqueKey)) {
      return false;
    }
    
    // Skip if this key has already been processed (unless force=true)
    if (!item.force && this.processedKeys.has(uniqueKey)) {
      return false;
    }
    
    // Calculate priority for this item
    const priority = this._calculatePriority(item);
    item.priority = priority;
    item.uniqueKey = uniqueKey; // Store the key for later reference
    item.addedAt = Date.now();  // Track when the item was added
    
    // Find the right position based on priority
    const insertIndex = this._findInsertIndex(priority);
    this.items.splice(insertIndex, 0, item);
    
    // Mark as processed to prevent duplicates
    this.processedKeys.add(uniqueKey);
    
    // Update statistics
    this.stats.enqueued++;
    
    return true;
  }

  /**
   * Remove and return the highest priority item from the queue
   * @return {Object|null} - The next item to process, or null if the queue is empty
   */
  dequeue() {
    if (this.isEmpty()) {
      return null;
    }
    
    const item = this.items.shift();
    const uniqueKey = item.uniqueKey;
    
    // Track that this item is now being processed
    this.processingMap.set(uniqueKey, {
      startTime: Date.now(),
      item: item
    });
    
    // Update statistics
    this.stats.dequeued++;
    
    return item;
  }

  /**
   * Mark an item as processed (successfully or with failure)
   * @param {Object} item - The processed item
   * @param {boolean} success - Whether processing was successful
   */
  markProcessed(item, success = true) {
    if (!item || !item.uniqueKey) return;
    
    const uniqueKey = item.uniqueKey;
    
    // Remove from processing map
    this.processingMap.delete(uniqueKey);
    
    if (success) {
      // Update success statistics
      this.stats.completed++;
    } else {
      // Handle failure case
      this.stats.failed++;
      
      // Retry logic for failed items
      const MAX_RETRIES = 3;
      const RETRY_DELAYS = [3000, 10000, 30000]; // 3s, 10s, 30s
      
      if (!item.retryCount || item.retryCount < MAX_RETRIES) {
        // Increment retry counter
        item.retryCount = (item.retryCount || 0) + 1;
        
        // Reduce priority for retried items
        item.priority = Math.max(1, item.priority - 20); 
        this.stats.retried++;
        
        // Re-add to queue after a delay
        setTimeout(() => {
          this.processedKeys.delete(uniqueKey); // Allow re-adding
          this.enqueue(item);
        }, RETRY_DELAYS[item.retryCount - 1] || 30000);
      }
    }
  }

  /**
   * Check if the queue is empty
   * @return {boolean} - True if the queue has no items
   */
  isEmpty() {
    return this.items.length === 0;
  }

  /**
   * Get the number of items in the queue
   * @return {number} - Number of queued items
   */
  size() {
    return this.items.length;
  }

  /**
   * Get comprehensive statistics about queue operations
   * @return {Object} - Queue statistics
   */
  getStats() {
    return {
      ...this.stats,
      queuedItems: this.items.length,
      processingItems: this.processingMap.size,
      uniqueKeysTracked: this.processedKeys.size,
      priorityMappings: this.priorityScores.size,
      oldestItem: this.items.length > 0 ? 
        Math.round((Date.now() - this.items[this.items.length - 1].addedAt) / 1000) + 's' : 
        'none',
      highestPriority: this.items.length > 0 ? this.items[0].priority : 'none'
    };
  }

  /**
   * Clear the queue and all tracking data
   */
  clear() {
    this.items = [];
    this.processingMap.clear();
    this.processedKeys.clear();
    this.priorityScores.clear();
    
    // Reset statistics
    this.stats = {
      enqueued: 0,
      dequeued: 0,
      completed: 0,
      failed: 0,
      retried: 0
    };
  }

  /**
   * Cleanup stalled items that have been in processing state too long
   * @param {number} maxAge - Maximum processing time in milliseconds (default: 5 minutes)
   * @return {number} - Number of items cleaned up
   */
  cleanupStalled(maxAge = 300000) {
    let cleanedCount = 0;
    const now = Date.now();
    
    for (const [key, info] of this.processingMap.entries()) {
      if (now - info.startTime > maxAge) {
        // Item has been processing too long, consider it failed
        this.processingMap.delete(key);
        cleanedCount++;
        
        // Potentially retry the item
        this.stats.failed++;
        
        if (!info.item.retryCount || info.item.retryCount < 3) {
          this.processedKeys.delete(key);
          info.item.retryCount = (info.item.retryCount || 0) + 1;
          this.stats.retried++;
          this.enqueue(info.item);
        }
      }
    }
    
    return cleanedCount;
  }

  /**
   * Calculate priority score for an item
   * @param {Object} item - The item to calculate priority for
   * @return {number} - Priority score (higher = more important)
   * @private
   */
  _calculatePriority(item) {
    // Base priority
    let score = 50;
    
    // Text length factor (shorter texts get higher priority)
    if (item.text) {
      if (item.text.length < 50) score += 20;
      else if (item.text.length > 500) score -= 20;
    }
    
    // Popular languages get priority
    const popularLanguages = ['en', 'de', 'fr', 'es', 'it', 'ru', 'zh', 'ja', 'ko'];
    if (popularLanguages.includes(item.targetLang)) score += 10;
    
    // Consider previously seen language/namespace combinations
    const keyBase = `${item.targetLang}:${item.namespace || 'common'}`;
    const prevScore = this.priorityScores.get(keyBase) || 0;
    score += prevScore;
    
    // Update priority for this language/namespace
    this.priorityScores.set(keyBase, prevScore + 5);
    
    // Force high priority items
    if (item.highPriority) score += 100;
    
    return score;
  }

  /**
   * Find the right position for inserting an item based on its priority
   * @param {number} priority - Priority score
   * @return {number} - Index to insert at
   * @private
   */
  _findInsertIndex(priority) {
    for (let i = 0; i < this.items.length; i++) {
      if (this.items[i].priority < priority) {
        return i;
      }
    }
    return this.items.length;
  }

  /**
   * Generate a hash key from text content when no explicit key is provided
   * @param {string} text - Text to hash
   * @return {string} - Hash representation
   * @private
   */
  _generateKeyFromText(text) {
    if (!text) return 'empty';
    
    // Simple hash function for text
    let hash = 0;
    for (let i = 0; i < text.length; i++) {
      const char = text.charCodeAt(i);
      hash = ((hash << 5) - hash) + char;
      hash = hash & hash;
    }
    
    return `text_${Math.abs(hash).toString(36)}`;
  }
}

module.exports = { Queue };