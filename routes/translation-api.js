// routes/translation-api.js - Updated for i18next compatibility

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

// Translation retry tracking
const translationRetryCounter = {};
const MAX_RETRIES = 3;

// Apply optional authentication to all routes
router.use(optionalAuth);

/**
 * Process background translation queue
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
  const queueKey = `${item.targetLang}:${item.context}:${item.text.substring(0, 20)}`;
  
  // Skip if too many retries
  if (translationRetryCounter[queueKey] && translationRetryCounter[queueKey] >= MAX_RETRIES) {
    console.warn(`Skipping translation after ${MAX_RETRIES} failed attempts: ${queueKey}`);
    isProcessing = false;
    
    // Process next item or wait
    if (!backgroundQueue.isEmpty()) {
      processBackgroundQueue();
    } else {
      setTimeout(processBackgroundQueue, 1000);
    }
    return;
  }
  
  // Process translation
  translateText(item.text, item.targetLang, item.context || '', item.sourceLang || 'en')
    .then(translation => {
      // Log success in development
      if (process.env.NODE_ENV === 'development') {
        console.log(`Background translation complete: [${item.targetLang}] ${item.text.substring(0, 30)}...`);
      }
      
      // Reset retry counter on success
      if (translationRetryCounter[queueKey]) {
        delete translationRetryCounter[queueKey];
      }
    })
    .catch(error => {
      console.error('Background translation error:', error);
      
      // Increment retry counter
      translationRetryCounter[queueKey] = (translationRetryCounter[queueKey] || 0) + 1;
      
      // Save error information for diagnostics
      saveTranslationError(item.text, item.targetLang, error.message, item.context);
      
      // Don't retry if network is offline
      if (!navigator?.onLine) {
        translationRetryCounter[queueKey] = MAX_RETRIES;
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
      console.error(`Translation Error [${lang}] ${context}: ${errorMsg} for text "${text.substring(0, 50)}..."`);
    }
  } catch (e) {
    console.error('Error saving translation error data:', e);
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
        fromSource: true
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
      console.log('Translation request:', requestInfo);
    }
    
    // Check if this translation is in the cache
    const cachedTranslation = await checkCache(text, targetLang, context || '');
    if (cachedTranslation) {
      // Add debug info in development
      if (process.env.NODE_ENV === 'development') {
        console.log(`Cache hit for [${targetLang}] "${text.substring(0, 30)}..."`);
      }
      
      return res.json({
        original: text,
        translated: cachedTranslation,
        language: targetLang,
        fromCache: true,
        requestInfo: process.env.NODE_ENV === 'development' ? requestInfo : undefined
      });
    }
    
    // If background flag is set, add to background queue and return immediately
    if (background) {
      backgroundQueue.enqueue({
        text,
        targetLang,
        context: context || '',
        sourceLang: sourceLang || 'en'
      });
      
      return res.json({
        original: text,
        translated: text, // Return original as placeholder
        language: targetLang,
        background: true,
        requestInfo: process.env.NODE_ENV === 'development' ? requestInfo : undefined
      });
    }
    
    // Otherwise, translate immediately
    const translatedText = await translateText(
      text, 
      targetLang, 
      context || '', 
      sourceLang || 'en'
    );
    
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
        console.error('Error updating metrics:', metricError);
      }
    }
    
    // For i18next compatibility, include key in response if provided
    const response = { 
      original: text, 
      translated: translatedText, 
      language: targetLang,
      requestInfo: process.env.NODE_ENV === 'development' ? requestInfo : undefined
    };
    
    if (req.body.key !== undefined) {
      response.key = req.body.key;
      response.ns = req.body.ns || 'common';
    }
    
    res.json(response);
  } catch (error) {
    console.error("Translation error:", error);
    
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
        fromSource: true
      });
    }
    
    // First check cache for all texts
    const results = [];
    const missingTexts = [];
    const missingIndices = [];
    
    // Log cache hits/misses in development
    const cacheStats = { hits: 0, misses: 0, empty: 0 };
    
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
        // Note missing translations
        missingTexts.push(text);
        missingIndices.push(i);
        // Set placeholder
        results[i] = text;
        cacheStats.misses++;
      }
    }
    
    // Log cache statistics in development
    if (process.env.NODE_ENV === 'development') {
      console.log(`Batch translation cache stats: ${JSON.stringify(cacheStats)}`);
    }
    
    // If all texts were in cache, return immediately
    if (missingTexts.length === 0) {
      const response = {
        translations: results,
        fromCache: true,
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
      missingTexts.forEach((text, idx) => {
        backgroundQueue.enqueue({
          text,
          targetLang,
          context: context || '',
          sourceLang: sourceLang || 'en'
        });
      });
      
      const response = {
        translations: results,
        background: true,
        missingCount: missingTexts.length,
        cacheStats: process.env.NODE_ENV === 'development' ? cacheStats : undefined
      };
      
      // For i18next compatibility
      if (req.body.keys) {
        response.keys = req.body.keys;
        response.ns = req.body.ns || 'common';
      }
      
      return res.json(response);
    }
    
    // Otherwise, translate missing texts now
    const translations = await batchTranslate(
      missingTexts, 
      targetLang, 
      context || '', 
      sourceLang || 'en'
    );
    
    // Merge into results
    for (let i = 0; i < translations.length; i++) {
      results[missingIndices[i]] = translations[i];
    }
    
    const response = { 
      translations: results,
      cacheStats: process.env.NODE_ENV === 'development' ? cacheStats : undefined
    };
    
    // For i18next compatibility
    if (req.body.keys) {
      response.keys = req.body.keys;
      response.ns = req.body.ns || 'common';
    }
    
    res.json(response);
  } catch (error) {
    console.error("Batch translation error:", error);
    
    // Log detailed error for diagnostics
    const errorDetails = {
      message: error.message,
      type: error.name,
      stack: process.env.NODE_ENV === 'development' ? error.stack : undefined,
      targetLang: req.body.targetLang || req.body.lng,
      context: req.body.context || req.body.ns,
      textsCount: req.body.texts?.length || req.body.keys?.length
    };
    
    console.error("Batch translation error details:", errorDetails);
    
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

// Export the router
module.exports = router;