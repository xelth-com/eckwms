// routes/translation-api.js
const express = require('express');
const router = express.Router();
const { translateText, batchTranslate, checkCache, saveToCache } = require('../services/translationService');
const { optionalAuth } = require('../middleware/auth');
const { Queue } = require('../utils/queue');

// Queue for background translations
const backgroundQueue = new Queue();
let isProcessing = false;

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
  
  // Process translation
  translateText(item.text, item.targetLang, item.context || '', item.sourceLang || 'en')
    .then(translation => {
      // Log success in development
      if (process.env.NODE_ENV === 'development') {
        console.log(`Background translation complete: [${item.targetLang}] ${item.text.substring(0, 30)}...`);
      }
    })
    .catch(error => {
      console.error('Background translation error:', error);
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
    
    // Check if this translation is in the cache
    const cachedTranslation = await checkCache(text, targetLang, context || '');
    if (cachedTranslation) {
      return res.json({
        original: text,
        translated: cachedTranslation,
        language: targetLang,
        fromCache: true
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
        background: true
      });
    }
    
    // Otherwise, translate immediately
    const translatedText = await translateText(
      text, 
      targetLang, 
      context || '', 
      sourceLang || 'en'
    );
    
    res.json({ 
      original: text, 
      translated: translatedText, 
      language: targetLang 
    });
  } catch (error) {
    console.error("Translation error:", error);
    res.status(500).json({ error: "Translation failed", details: error.message });
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
    
    for (let i = 0; i < texts.length; i++) {
      const text = texts[i];
      
      // Skip empty texts
      if (!text || text.trim() === '') {
        results[i] = text;
        continue;
      }
      
      // Check cache
      const cachedTranslation = await checkCache(text, targetLang, context || '');
      if (cachedTranslation) {
        results[i] = cachedTranslation;
      } else {
        // Note missing translations
        missingTexts.push(text);
        missingIndices.push(i);
        // Set placeholder
        results[i] = text;
      }
    }
    
    // If all texts were in cache, return immediately
    if (missingTexts.length === 0) {
      return res.json({
        translations: results,
        fromCache: true
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
        missingCount: missingTexts.length
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
      translations: results
    });
  } catch (error) {
    console.error("Batch translation error:", error);
    res.status(500).json({ error: "Batch translation failed", details: error.message });
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
    
    // Currently returns a simple response since detection is not fully implemented
    res.json({ 
      text: text,
      detectedLanguage: 'auto',
      message: 'Language detection not implemented yet'
    });
  } catch (error) {
    console.error("Language detection error:", error);
    res.status(500).json({ error: "Language detection failed", details: error.message });
  }
});

module.exports = router;