/**
 * translationFileCache.js 
 * Shared cache system for tracking missing translation files
 * to prevent repeated 404 errors and network requests
 */

(function() {
  // Create global cache if it doesn't exist
  if (!window.translationFileCache) {
    window.translationFileCache = {
      // Storage for missing files information with timestamps
      missingFiles: {},
      
      // Cache expiration time - 5 minutes
      cacheExpiry: 5 * 60 * 1000,
      
      /**
       * Check if a file is marked as missing
       * @param {string} fileKey - File identifier (e.g., "de:common")
       * @returns {boolean} - True if file is known to be missing
       */
      isFileMissing: function(fileKey) {
        if (!this.missingFiles[fileKey]) {
          return false;
        }
        
        // Check if cache has expired
        const timestamp = this.missingFiles[fileKey];
        const now = Date.now();
        
        if (now - timestamp > this.cacheExpiry) {
          delete this.missingFiles[fileKey];
          return false;
        }
        
        return true;
      },
      
      /**
       * Mark a file as missing
       * @param {string} fileKey - File identifier (e.g., "de:common")
       */
      markAsMissing: function(fileKey) {
        this.missingFiles[fileKey] = Date.now();
        
        // For backward compatibility with existing code
        if (!window.missingTranslationFiles) {
          window.missingTranslationFiles = new Set();
        }
        window.missingTranslationFiles.add(fileKey);
      },
      
      /**
       * Force reset cache for a specific file
       * @param {string} fileKey - File identifier to reset
       */
      resetFile: function(fileKey) {
        delete this.missingFiles[fileKey];
        if (window.missingTranslationFiles) {
          window.missingTranslationFiles.delete(fileKey);
        }
      },
      
      /**
       * Reset all cached information
       */
      resetAll: function() {
        this.missingFiles = {};
        if (window.missingTranslationFiles) {
          window.missingTranslationFiles.clear();
        }
      },
      
      /**
       * Silently load a translation file without generating 404 errors
       * @param {string} language - Language code
       * @param {string} namespace - Translation namespace
       * @returns {Promise<Object|null>} - Translation object or null if not found
       */
      loadFile: async function(language, namespace) {
        const fileKey = `${language}:${namespace}`;
        
        // Skip if already known to be missing
        if (this.isFileMissing(fileKey)) {
          return null;
        }
        
        try {
          // Use HEAD request first to check if file exists without triggering 404 in console
          const checkResponse = await fetch(`/locales/${language}/${namespace}.json`, {
            method: 'HEAD',
            cache: 'no-cache' // Don't cache HEAD requests
          });
          
          // If file doesn't exist, remember that
          if (!checkResponse.ok) {
            this.markAsMissing(fileKey);
            return null;
          }
          
          // File exists, now actually load it
          const response = await fetch(`/locales/${language}/${namespace}.json`);
          if (!response.ok) {
            this.markAsMissing(fileKey);
            return null;
          }
          
          // Parse and return the translation data
          return await response.json();
        } catch (error) {
          // Silently mark this file as missing
          this.markAsMissing(fileKey);
          return null;
        }
      }
    };
    
    // Set up periodic cache reset (every 5 minutes)
    setInterval(() => {
      console.log("[i18n] Resetting translation file cache to check for new files");
      window.translationFileCache.resetAll();
    }, 5 * 60 * 1000);
  }
  
  // Export for module systems if needed
  if (typeof module !== 'undefined' && module.exports) {
    module.exports = window.translationFileCache;
  }
})();