// routes/translation-api.js
const express = require('express');
const router = express.Router();
const { translateText, batchTranslate, checkCache, saveToCache } = require('../services/translationService');
const { optionalAuth } = require('../middleware/auth');
const { Queue } = require('../utils/queue');

// PostgreSQL model for translation error logging
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
      if (!navigator.onLine) {
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
 * Translate a single text
 * POST /api/translate
 * Body: { text: string, targetLang: string, context: string, background: boolean }
 */
router.post('/translate', async (req, res) => {
  try {
    const { text, targetLang, context, background, htmlContent, sourceLang } = req.body;
    
    if (!text || !targetLang) {
      return res.status(400).json({ error: 'Text and target language required' });
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
    
    res.json({ 
      original: text, 
      translated: translatedText, 
      language: targetLang,
      requestInfo: process.env.NODE_ENV === 'development' ? requestInfo : undefined
    });
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
 * Batch translate multiple texts
 * POST /api/translate-batch
 * Body: { texts: string[], targetLang: string, context: string, background: boolean }
 */
router.post('/translate-batch', async (req, res) => {
  try {
    const { texts, targetLang, context, background, htmlContent, sourceLang } = req.body;
    
    if (!texts || !Array.isArray(texts) || !targetLang) {
      return res.status(400).json({ error: 'Array of texts and target language required' });
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
      return res.json({
        translations: results,
        fromCache: true,
        cacheStats: process.env.NODE_ENV === 'development' ? cacheStats : undefined
      });
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
      
      return res.json({
        translations: results,
        background: true,
        missingCount: missingTexts.length,
        cacheStats: process.env.NODE_ENV === 'development' ? cacheStats : undefined
      });
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
    
    res.json({ 
      translations: results,
      cacheStats: process.env.NODE_ENV === 'development' ? cacheStats : undefined
    });
  } catch (error) {
    console.error("Batch translation error:", error);
    
    // Log detailed error for diagnostics
    const errorDetails = {
      message: error.message,
      type: error.name,
      stack: process.env.NODE_ENV === 'development' ? error.stack : undefined,
      targetLang: req.body.targetLang,
      context: req.body.context,
      textsCount: req.body.texts?.length
    };
    
    console.error("Batch translation error details:", errorDetails);
    
    // Save at most 3 sample texts for error analysis
    if (req.body.texts && req.body.texts.length > 0) {
      const sampleCount = Math.min(3, req.body.texts.length);
      for (let i = 0; i < sampleCount; i++) {
        saveTranslationError(
          req.body.texts[i], 
          req.body.targetLang || 'unknown', 
          `Batch error: ${error.message}`, 
          req.body.context || 'batch'
        );
      }
    }
    
    res.status(500).json({ 
      error: "Batch translation failed", 
      details: error.message,
      failedCount: req.body.texts?.length || 0,
      retryRecommended: !(error.message.includes('quota') || error.message.includes('limit'))
    });
  }
});

/**
 * Language detection endpoint for future use
 * POST /api/detect-language
 * Body: { text: string }
 */
router.post('/detect-language', async (req, res) => {
  try {
    const { text } = req.body;
    
    if (!text) {
      return res.status(400).json({ error: 'Text required' });
    }
    
    // Add some basic heuristics for language detection
    const languagePatterns = {
      ru: /[а-яА-ЯёЁ]{3,}/,
      de: /[äöüÄÖÜß]/,
      fr: /[àâçéèêëîïôùûüÿœæ]/i,
      zh: /[\u4e00-\u9fa5]/,
      ja: /[\u3040-\u30ff\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff\uff66-\uff9f]/,
      ko: /[\uac00-\ud7af\u1100-\u11ff\u3130-\u318f\ua960-\ua97f\ud7b0-\ud7ff]/,
      ar: /[\u0600-\u06ff\u0750-\u077f\u08a0-\u08ff\ufb50-\ufdff\ufe70-\ufefc]/
    };
    
    // Simple detection by character patterns
    let detectedLanguage = 'en'; // Default
    let maxMatches = 0;
    
    for (const [lang, pattern] of Object.entries(languagePatterns)) {
      const matches = (text.match(pattern) || []).length;
      if (matches > maxMatches) {
        maxMatches = matches;
        detectedLanguage = lang;
      }
    }
    
    res.json({
      text: text,
      detectedLanguage: maxMatches > 0 ? detectedLanguage : 'auto',
      confidence: maxMatches > 0 ? Math.min(maxMatches / 5, 0.9) : 0,
      message: 'Basic language detection based on character patterns'
    });
  } catch (error) {
    console.error("Language detection error:", error);
    res.status(500).json({ error: "Language detection failed", details: error.message });
  }
});

/**
 * Translation statistics endpoint (admin only)
 * GET /api/translation-stats
 */
router.get('/translation-stats', optionalAuth, async (req, res) => {
  // Check if user is admin
  if (!req.user || req.user.role !== 'admin') {
    return res.status(403).json({ error: 'Admin access required' });
  }
  
  try {
    // Get global metrics
    const metrics = global.translationMetrics || {
      totalTranslations: 0,
      byLanguage: {}
    };
    
    // Get cache stats if TranslationCache is available
    let dbStats = null;
    if (TranslationCache) {
      const totalCount = await TranslationCache.count();
      const languageCounts = await TranslationCache.findAll({
        attributes: ['language', [sequelize.fn('COUNT', sequelize.col('id')), 'count']],
        group: ['language']
      });
      
      dbStats = {
        totalCached: totalCount,
        byLanguage: languageCounts.reduce((acc, item) => {
          acc[item.language] = parseInt(item.dataValues.count);
          return acc;
        }, {})
      };
    }
    
    // Get queue info
    const queueInfo = {
      pendingTranslations: backgroundQueue.size(),
      isProcessing: isProcessing,
      failedAttempts: Object.keys(translationRetryCounter).length
    };
    
    res.json({
      metrics,
      cacheStats: dbStats,
      queueInfo,
      memoryUsage: process.memoryUsage()
    });
  } catch (error) {
    console.error("Error getting translation stats:", error);
    res.status(500).json({ error: "Failed to get translation statistics", details: error.message });
  }
});

module.exports = router;