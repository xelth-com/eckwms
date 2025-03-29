// routes/translation-api.js - Optimized for i18next compatibility and multi-user concurrency

const express = require('express');
const router = express.Router();
const { translateText, batchTranslate, checkCache, saveToCache } = require('../services/translationService');
const { optionalAuth } = require('../middleware/auth');
const { Queue } = require('../utils/queue');

// PostgreSQL model for translation caching
let TranslationCache;
try {
  const { sequelize } = require('../models/postgresql');
  if (sequelize) {
    TranslationCache = require('../models/postgresql').TranslationCache;
  }
} catch (error) {
  console.warn('PostgreSQL not configured for translation error logging');
}

// Queue for background translations
const backgroundQueue = new Queue();
let isProcessing = false;

// Thread-safe map of in-progress translations to prevent duplicate requests
const translationProcessingMap = new Map();

// Periodically clean up stale entries (translations that started but never completed)
setInterval(() => {
  const now = Date.now();
  const STALE_THRESHOLD = 5 * 60 * 1000; // 5 minutes
  
  let cleanedCount = 0;
  translationProcessingMap.forEach((entry, key) => {
    if (now - entry.startTime > STALE_THRESHOLD) {
      console.log(`[i18n] Removing stale translation task: ${key}`);
      translationProcessingMap.delete(key);
      cleanedCount++;
    }
  });
  
  if (cleanedCount > 0) {
    console.log(`[i18n] Cleaned up ${cleanedCount} stale translation tasks`);
  }
}, 60 * 1000); // Check every minute

// Translation retry tracking
const translationRetryCounter = {};
const MAX_RETRIES = 3;

// Apply optional authentication to all routes
router.use(optionalAuth);

/**
 * Process background translation queue with enhanced tracking
 */
function processBackgroundQueue() {
  if (isProcessing || backgroundQueue.isEmpty()) {
    // If already processing or no items, schedule next check
    setTimeout(processBackgroundQueue, 1000);
    return;
  }
  
  isProcessing = true;
  
  // Get next item
  const item = backgroundQueue.dequeue();
  const queueKey = item.queueKey || `${item.targetLang}:${item.context || 'common'}:${item.text.substring(0, 20)}`;
  
  // Skip if too many retries
  if (translationRetryCounter[queueKey] && translationRetryCounter[queueKey] >= MAX_RETRIES) {
    console.warn(`[i18n] Skipping translation after ${MAX_RETRIES} failed attempts: ${queueKey}`);
    
    // Clean up tracking entries
    if (translationProcessingMap.has(queueKey)) {
      translationProcessingMap.delete(queueKey);
    }
    
    isProcessing = false;
    
    // Process next item or wait
    if (!backgroundQueue.isEmpty()) {
      processBackgroundQueue();
    } else {
      setTimeout(processBackgroundQueue, 1000);
    }
    return;
  }
  
  // Check cache one more time before translation
  checkCache(item.text, item.targetLang, item.context || '')
    .then(cachedResult => {
      if (cachedResult) {
        console.log(`[i18n] Found in cache during queue processing: [${item.targetLang}] ${item.text.substring(0, 30)}...`);
        
        // Clean up tracking entries since we don't need to translate
        if (translationProcessingMap.has(queueKey)) {
          translationProcessingMap.delete(queueKey);
        }
        
        return null; // Skip translation
      }
      
      // Process translation
      return translateText(item.text, item.targetLang, item.context || '', item.sourceLang || 'en');
    })
    .then(translation => {
      if (translation === null) {
        // Cached result was found, nothing more to do
        return;
      }
      
      // Log success in development
      console.log(`[i18n] Background translation complete: [${item.targetLang}] ${item.text.substring(0, 30)}...`);
      
      // Reset retry counter on success
      if (translationRetryCounter[queueKey]) {
        delete translationRetryCounter[queueKey];
      }
      
      // Remove from processing map when done
      if (translationProcessingMap.has(queueKey)) {
        translationProcessingMap.delete(queueKey);
      }
    })
    .catch(error => {
      console.error('[i18n] Background translation error:', error);
      
      // Increment retry counter
      translationRetryCounter[queueKey] = (translationRetryCounter[queueKey] || 0) + 1;
      
      // Save error information for diagnostics
      saveTranslationError(item.text, item.targetLang, error.message, item.context);
      
      // Handle error case for processing map
      if (translationProcessingMap.has(queueKey)) {
        // Only remove after MAX_RETRIES
        const entry = translationProcessingMap.get(queueKey);
        if ((entry.retryCount || 0) >= MAX_RETRIES - 1) {
          translationProcessingMap.delete(queueKey);
        } else {
          // Update retry count
          entry.retryCount = (entry.retryCount || 0) + 1;
          translationProcessingMap.set(queueKey, entry);
        }
      }
    })
    .finally(() => {
      isProcessing = false;
      
      // Process next item if queue not empty, otherwise schedule next check
      if (!backgroundQueue.isEmpty()) {
        processBackgroundQueue();
      } else {
        setTimeout(processBackgroundQueue, 1000);
      }
    });
}

// Start background processing
processBackgroundQueue();

/**
 * Save information about translation errors for diagnostics
 * @param {string} text - Original text
 * @param {string} lang - Target language
 * @param {string} errorMsg - Error message
 * @param {string} context - Translation context
 */
async function saveTranslationError(text, lang, errorMsg, context = '') {
  try {
    if (TranslationCache) {
      await TranslationCache.create({
        key: `error_${Date.now()}`,
        language: lang,
        originalText: text.substring(0, 200), // Limit text size
        translatedText: '',
        context: context || 'error',
        errorMessage: errorMsg,
        createdAt: new Date()
      });
    } else {
      // Log to console if database not available
      console.error(`[i18n] Translation Error [${lang}] ${context}: ${errorMsg} for text "${text.substring(0, 50)}..."`);
    }
  } catch (e) {
    console.error('[i18n] Error saving translation error data:', e);
  }
}

/**
 * Handle single text translation - i18next compatible endpoint
 * POST /api/translate
 * Body formats supported:
 * 1. i18next format: { lng: "de", ns: "common", key: "welcome", defaultValue: "Welcome" }
 * 2. Legacy format: { text: "Welcome", targetLang: "de", context: "common", background: false }
 */
router.post('/translate', async (req, res) => {
  try {
    // Support both i18next format and legacy format
    let text, targetLang, context, background, sourceLang;
    
    // Check for i18next format
    if (req.body.lng && req.body.key !== undefined) {
      // i18next format
      targetLang = Array.isArray(req.body.lng) ? req.body.lng[0] : req.body.lng;
      text = req.body.defaultValue || req.body.key;
      context = req.body.ns || 'common';
      background = req.body.background || false;
      sourceLang = process.env.DEFAULT_LANGUAGE || 'en';
    } else {
      // Legacy format
      text = req.body.text;
      targetLang = req.body.targetLang;
      context = req.body.context;
      background = req.body.background || false;
      sourceLang = req.body.sourceLang || process.env.DEFAULT_LANGUAGE || 'en';
    }
    
    // Ensure required parameters are present
    if (!text || !targetLang) {
      return res.status(400).json({ error: 'Text and target language required' });
    }
    
    // Skip translation for default language (just return original)
    if (targetLang === (process.env.DEFAULT_LANGUAGE || 'en')) {
      return res.json({
        original: text,
        translated: text,
        language: targetLang,
        fromSource: true,
        status: 'complete'
      });
    }
    
    // Add request information for debugging
    const requestInfo = {
      timestamp: new Date().toISOString(),
      ip: req.ip,
      userAgent: req.get('User-Agent'),
      textLength: text.length,
      language: targetLang
    };
    
    if (process.env.NODE_ENV === 'development') {
      console.log('[i18n] Translation request:', requestInfo);
    }
    
    // Always check cache first, with comprehensive logging
    const cachedTranslation = await checkCache(text, targetLang, context || '');
    if (cachedTranslation) {
      // Add debug info in development
      if (process.env.NODE_ENV === 'development') {
        console.log(`[i18n] Cache hit for [${targetLang}] "${text.substring(0, 30)}..."`);
      }
      
      return res.json({
        original: text,
        translated: cachedTranslation,
        language: targetLang,
        fromCache: true,
        status: 'complete',
        requestInfo: process.env.NODE_ENV === 'development' ? requestInfo : undefined
      });
    }
    
    // Generate queueKey for tracking
    const queueKey = `${targetLang}:${context || 'common'}:${text.substring(0, 20)}`;
    
    // Check if this translation is already being processed
    if (translationProcessingMap.has(queueKey)) {
      console.log(`[i18n] Translation already in progress: ${queueKey}`);
      
      // Return a "pending" response with retry-after header
      res.setHeader('Retry-After', '3');
      return res.status(202).json({
        original: text,
        translated: text, // Return original as placeholder
        language: targetLang,
        status: 'pending',
        retryAfter: 3,
        requestInfo: process.env.NODE_ENV === 'development' ? requestInfo : undefined
      });
    }
    
    // If background flag is set, add to background queue and return immediately
    if (background) {
      // Add entry to processing map
      translationProcessingMap.set(queueKey, {
        startTime: Date.now(),
        text: text.substring(0, 30) + '...',
        retryCount: 0
      });
      
      backgroundQueue.enqueue({
        text,
        targetLang,
        context: context || '',
        sourceLang: sourceLang || 'en',
        queueKey: queueKey  // Add reference to track completion
      });
      
      res.setHeader('Retry-After', '3');
      return res.status(202).json({
        original: text,
        translated: text, // Return original as placeholder
        language: targetLang,
        status: 'pending',
        retryAfter: 3,
        requestInfo: process.env.NODE_ENV === 'development' ? requestInfo : undefined
      });
    }
    
    // Otherwise, translate immediately
    // Add entry to processing map during translation
    translationProcessingMap.set(queueKey, {
      startTime: Date.now(),
      text: text.substring(0, 30) + '...',
      retryCount: 0
    });
    
    const translatedText = await translateText(
      text, 
      targetLang, 
      context || '', 
      sourceLang || 'en'
    );
    
    // Remove from processing map when done
    translationProcessingMap.delete(queueKey);
    
    // Add usage statistics
    if (translatedText !== text) {
      try {
        // Track successful translations for metrics if needed
        // This could be extended to save to database
        global.translationMetrics = global.translationMetrics || {
          totalTranslations: 0,
          byLanguage: {}
        };
        
        global.translationMetrics.totalTranslations++;
        global.translationMetrics.byLanguage[targetLang] = 
          (global.translationMetrics.byLanguage[targetLang] || 0) + 1;
      } catch (metricError) {
        console.error('[i18n] Error updating metrics:', metricError);
      }
    }
    
    // For i18next compatibility, include key in response if provided
    const response = { 
      original: text, 
      translated: translatedText, 
      language: targetLang,
      status: 'complete',
      requestInfo: process.env.NODE_ENV === 'development' ? requestInfo : undefined
    };
    
    if (req.body.key !== undefined) {
      response.key = req.body.key;
      response.ns = req.body.ns || 'common';
    }
    
    res.json(response);
  } catch (error) {
    console.error("[i18n] Translation error:", error);
    
    // Log the error for diagnostic purposes
    saveTranslationError(req.body.text || '', req.body.targetLang || 'unknown', error.message, req.body.context || '');
    
    // Provide detailed error response
    res.status(500).json({ 
      error: "Translation failed", 
      details: error.message,
      stack: process.env.NODE_ENV === 'development' ? error.stack : undefined,
      retryRecommended: !(error.message.includes('quota') || error.message.includes('limit'))
    });
  }
});

/**
 * Batch translate multiple texts - i18next compatible
 * POST /api/translate-batch
 * Body formats supported:
 * 1. i18next format: { lng: "de", ns: "common", keys: ["welcome", "hello"], defaultValues: ["Welcome", "Hello"] }
 * 2. Legacy format: { texts: ["Welcome", "Hello"], targetLang: "de", context: "common", background: false }
 */
router.post('/translate-batch', async (req, res) => {
  try {
    // Support both i18next format and legacy format
    let texts, targetLang, context, background, sourceLang;
    
    // Check for i18next format
    if (req.body.lng && req.body.keys) {
      // i18next format
      targetLang = Array.isArray(req.body.lng) ? req.body.lng[0] : req.body.lng;
      
      // Prefer defaultValues if provided, otherwise use keys as texts
      if (req.body.defaultValues && Array.isArray(req.body.defaultValues)) {
        texts = req.body.defaultValues;
      } else {
        texts = req.body.keys;
      }
      
      context = req.body.ns || 'common';
      background = req.body.background || false;
      sourceLang = process.env.DEFAULT_LANGUAGE || 'en';
    } else {
      // Legacy format
      texts = req.body.texts;
      targetLang = req.body.targetLang;
      context = req.body.context;
      background = req.body.background || false;
      sourceLang = req.body.sourceLang || process.env.DEFAULT_LANGUAGE || 'en';
    }
    
    if (!texts || !Array.isArray(texts) || !targetLang) {
      return res.status(400).json({ error: 'Array of texts and target language required' });
    }
    
    // Skip translation for default language (just return originals)
    if (targetLang === (process.env.DEFAULT_LANGUAGE || 'en')) {
      return res.json({
        translations: texts,
        language: targetLang,
        fromSource: true,
        status: 'complete'
      });
    }
    
    // First check cache for all texts
    const results = [];
    const missingTexts = [];
    const missingIndices = [];
    const queuedItems = [];
    
    // Log cache hits/misses in development
    const cacheStats = { hits: 0, misses: 0, empty: 0, inProgress: 0 };
    
    for (let i = 0; i < texts.length; i++) {
      const text = texts[i];
      
      // Skip empty texts
      if (!text || text.trim() === '') {
        results[i] = text;
        cacheStats.empty++;
        continue;
      }
      
      // Check cache
      const cachedTranslation = await checkCache(text, targetLang, context || '');
      if (cachedTranslation) {
        results[i] = cachedTranslation;
        cacheStats.hits++;
      } else {
        // Generate queue key for tracking
        const queueKey = `${targetLang}:${context || 'common'}:${text.substring(0, 20)}`;
        
        // Check if already in progress
        if (translationProcessingMap.has(queueKey)) {
          // Already in progress, mark as pending
          results[i] = text; // Use original as placeholder
          cacheStats.inProgress++;
          
          // Track for response
          queuedItems.push({
            index: i,
            queueKey: queueKey
          });
        } else {
          // Note missing translations
          missingTexts.push(text);
          missingIndices.push(i);
          // Set placeholder
          results[i] = text;
          cacheStats.misses++;
        }
      }
    }
    
    // Log cache statistics in development
    if (process.env.NODE_ENV === 'development') {
      console.log(`[i18n] Batch translation cache stats: ${JSON.stringify(cacheStats)}`);
    }
    
    // If we have items in progress but not missing, return with pending status
    if (missingTexts.length === 0 && queuedItems.length > 0) {
      res.setHeader('Retry-After', '3');
      
      const response = {
        translations: results,
        pendingIndices: queuedItems.map(item => item.index),
        status: 'partial',
        retryAfter: 3,
        cacheStats: process.env.NODE_ENV === 'development' ? cacheStats : undefined
      };
      
      // For i18next compatibility
      if (req.body.keys) {
        response.keys = req.body.keys;
        response.ns = req.body.ns || 'common';
      }
      
      return res.status(202).json(response);
    }
    
    // If all texts were in cache, return immediately
    if (missingTexts.length === 0) {
      const response = {
        translations: results,
        fromCache: true,
        status: 'complete',
        cacheStats: process.env.NODE_ENV === 'development' ? cacheStats : undefined
      };
      
      // For i18next compatibility
      if (req.body.keys) {
        response.keys = req.body.keys;
        response.ns = req.body.ns || 'common';
      }
      
      return res.json(response);
    }
    
    // If background mode enabled, queue missing translations and return partial results
    if (background) {
      // Add all missing texts to queue and processing map
      missingTexts.forEach((text, idx) => {
        const queueKey = `${targetLang}:${context || 'common'}:${text.substring(0, 20)}`;
        
        // Add to processing map
        translationProcessingMap.set(queueKey, {
          startTime: Date.now(),
          text: text.substring(0, 30) + '...',
          retryCount: 0
        });
        
        // Add to queue
        backgroundQueue.enqueue({
          text,
          targetLang,
          context: context || '',
          sourceLang: sourceLang || 'en',
          queueKey: queueKey
        });
        
        // Track for response
        queuedItems.push({
          index: missingIndices[idx],
          queueKey: queueKey
        });
      });
      
      res.setHeader('Retry-After', '3');
      
      const response = {
        translations: results,
        status: 'partial',
        pendingIndices: queuedItems.map(item => item.index),
        retryAfter: 3,
        cacheStats: process.env.NODE_ENV === 'development' ? cacheStats : undefined
      };
      
      // For i18next compatibility
      if (req.body.keys) {
        response.keys = req.body.keys;
        response.ns = req.body.ns || 'common';
      }
      
      return res.status(202).json(response);
    }
    
    // Otherwise, translate missing texts now
    const queueKeys = [];
    
    // Add all texts to processing map first
    missingTexts.forEach((text, idx) => {
      const queueKey = `${targetLang}:${context || 'common'}:${text.substring(0, 20)}`;
      queueKeys.push(queueKey);
      
      // Add to processing map
      translationProcessingMap.set(queueKey, {
        startTime: Date.now(),
        text: text.substring(0, 30) + '...',
        retryCount: 0
      });
    });
    
    try {
      const translations = await batchTranslate(
        missingTexts, 
        targetLang, 
        context || '', 
        sourceLang || 'en'
      );
      
      // Remove from processing map when done
      queueKeys.forEach(key => {
        translationProcessingMap.delete(key);
      });
      
      // Merge into results
      for (let i = 0; i < translations.length; i++) {
        results[missingIndices[i]] = translations[i];
      }
      
      const response = { 
        translations: results,
        status: 'complete',
        cacheStats: process.env.NODE_ENV === 'development' ? cacheStats : undefined
      };
      
      // For i18next compatibility
      if (req.body.keys) {
        response.keys = req.body.keys;
        response.ns = req.body.ns || 'common';
      }
      
      res.json(response);
    } catch (error) {
      // Clean up processing map on error
      queueKeys.forEach(key => {
        translationProcessingMap.delete(key);
      });
      
      throw error; // Re-throw to be caught by outer catch
    }
  } catch (error) {
    console.error("[i18n] Batch translation error:", error);
    
    // Log detailed error for diagnostics
    const errorDetails = {
      message: error.message,
      type: error.name,
      stack: process.env.NODE_ENV === 'development' ? error.stack : undefined,
      targetLang: req.body.targetLang || req.body.lng,
      context: req.body.context || req.body.ns,
      textsCount: req.body.texts?.length || req.body.keys?.length
    };
    
    console.error("[i18n] Batch translation error details:", errorDetails);
    
    // Save at most 3 sample texts for error analysis
    const textsToAnalyze = req.body.texts || req.body.defaultValues || req.body.keys || [];
    if (textsToAnalyze.length > 0) {
      const sampleCount = Math.min(3, textsToAnalyze.length);
      for (let i = 0; i < sampleCount; i++) {
        saveTranslationError(
          textsToAnalyze[i], 
          req.body.targetLang || req.body.lng || 'unknown', 
          `Batch error: ${error.message}`, 
          req.body.context || req.body.ns || 'batch'
        );
      }
    }
    
    res.status(500).json({ 
      error: "Batch translation failed", 
      details: error.message,
      failedCount: textsToAnalyze.length || 0,
      retryRecommended: !(error.message.includes('quota') || error.message.includes('limit'))
    });
  }
});

// API endpoint to check translation status
router.get('/translation-status', (req, res) => {
  try {
    const queueSize = backgroundQueue.size();
    const processingCount = translationProcessingMap.size;
    
    // Get queue and processing statistics
    const queueStats = backgroundQueue.getStats();
    
    // Get languages being processed
    const languages = new Set();
    translationProcessingMap.forEach((info, key) => {
      const parts = key.split(':');
      if (parts.length > 0) {
        languages.add(parts[0]);
      }
    });
    
    // Return status information
    res.json({
      queueSize,
      processingCount,
      activeLanguages: Array.from(languages),
      queueStats: queueStats,
      health: queueSize < 100 && processingCount < 20 ? 'good' : 'busy'
    });
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

// Export the router
module.exports = router;