// Message deduplication utility to prevent duplicate processing
// Tracks processed message IDs with automatic cleanup

const processedCache = new Map();
const CACHE_TTL = 300000; // 5 minutes in milliseconds
const MAX_CACHE_SIZE = 10000; // Maximum number of entries before cleanup

/**
 * Checks if a message ID has already been processed
 * @param {string} msgId - Unique message identifier
 * @returns {boolean} - True if message is a duplicate, false otherwise
 */
function isDuplicate(msgId) {
    if (!msgId) {
        // If no msgId provided, treat as non-duplicate (allow processing)
        console.warn('[MessageDeduplicator] No msgId provided, treating as unique message');
        return false;
    }

    const now = Date.now();

    // Check if message already exists in cache
    if (processedCache.has(msgId)) {
        const timestamp = processedCache.get(msgId);

        // Check if the cached entry is still valid (within TTL)
        if (now - timestamp < CACHE_TTL) {
            console.log(`[MessageDeduplicator] Duplicate message detected: ${msgId}`);
            return true;
        } else {
            // Entry expired, remove it
            processedCache.delete(msgId);
        }
    }

    // Cleanup old entries if cache is too large
    if (processedCache.size >= MAX_CACHE_SIZE) {
        cleanupExpiredEntries(now);
    }

    // Add new message to cache
    processedCache.set(msgId, now);
    console.log(`[MessageDeduplicator] New message cached: ${msgId} (cache size: ${processedCache.size})`);
    return false;
}

/**
 * Removes expired entries from the cache
 * @param {number} now - Current timestamp
 */
function cleanupExpiredEntries(now) {
    const initialSize = processedCache.size;
    let removedCount = 0;

    for (const [msgId, timestamp] of processedCache.entries()) {
        if (now - timestamp >= CACHE_TTL) {
            processedCache.delete(msgId);
            removedCount++;
        }
    }

    console.log(`[MessageDeduplicator] Cleanup: removed ${removedCount} expired entries (${initialSize} -> ${processedCache.size})`);
}

/**
 * Manually clear the entire cache (useful for testing)
 */
function clearCache() {
    processedCache.clear();
    console.log('[MessageDeduplicator] Cache cleared');
}

/**
 * Get current cache statistics
 * @returns {object} - Cache statistics
 */
function getCacheStats() {
    return {
        size: processedCache.size,
        maxSize: MAX_CACHE_SIZE,
        ttl: CACHE_TTL
    };
}

module.exports = {
    isDuplicate,
    clearCache,
    getCacheStats
};
