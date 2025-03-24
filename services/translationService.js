// services/translationService.js
const { OpenAI } = require('openai');
const crypto = require('crypto');
const path = require('path');


// Initialize OpenAI API
const openai = new OpenAI({
  apiKey: process.env.OPENAI_API_KEY
});

// PostgreSQL model for translation caching
// Import only if DB connection exists
let TranslationCache;
try {
  const { sequelize } = require('../models/postgresql');
  if (sequelize) {
    TranslationCache = require('../models/postgresql').TranslationCache;
  }
} catch (error) {
  console.warn('PostgreSQL not configured for translation cache. Using file-based cache only.');
}



// In-memory cache for faster lookup of recent translations
const memoryCache = new Map();
const MEMORY_CACHE_MAX_SIZE = 1000; // Limit memory cache size

/**
 * Generate unique key for text
 * @param {string} text - Source text
 * @param {string} context - Translation context
 * @returns {string} - MD5 hash
 */
function generateKey(text, context = '') {
  return crypto
    .createHash('md5')
    .update(`${text}_${context}`)
    .digest('hex');
}

/**
 * Check translation cache in memory and database
 * @param {string} text - Source text
 * @param {string} targetLang - Target language
 * @param {string} context - Translation context
 * @returns {Promise<string|null>} - Translation or null if not found
 */
async function checkCache(text, targetLang, context = '') {
  const key = generateKey(text, context);
  const cacheKey = `${targetLang}:${key}`;
  
  // Check memory cache first for fastest access
  if (memoryCache.has(cacheKey)) {
    return memoryCache.get(cacheKey);
  }
  
  // Then check database cache if available
  if (TranslationCache) {
    try {
      const cachedTranslation = await TranslationCache.findOne({
        where: {
          key,
          language: targetLang
        }
      });
      
      if (cachedTranslation) {
        // Update usage metrics
        await cachedTranslation.update({
          lastUsed: new Date(),
          useCount: cachedTranslation.useCount + 1
        });
        
        // Add to memory cache for faster future access
        addToMemoryCache(cacheKey, cachedTranslation.translatedText);
        
        return cachedTranslation.translatedText;
      }
    } catch (error) {
      console.error('Error checking database cache:', error);
    }
  }
  
  return null;
}

// Удаляем функции для работы с файловым кешем

/**
 * Add translation to memory cache with size limit
 * @param {string} key - Cache key
 * @param {string} value - Translation
 */
function addToMemoryCache(key, value) {
  // If cache is full, remove oldest entries
  if (memoryCache.size >= MEMORY_CACHE_MAX_SIZE) {
    // Remove 10% of oldest entries
    const entriesToRemove = Math.floor(MEMORY_CACHE_MAX_SIZE * 0.1);
    const keys = [...memoryCache.keys()].slice(0, entriesToRemove);
    keys.forEach(k => memoryCache.delete(k));
  }
  
  memoryCache.set(key, value);
}

/**
 * Save translation to memory cache and database
 * @param {string} text - Source text
 * @param {string} targetLang - Target language (must be a string)
 * @param {string} translatedText - Translated text
 * @param {string} context - Translation context
 */
async function saveToCache(text, targetLang, translatedText, context = '') {
  // Make sure targetLang is a string
  if (Array.isArray(targetLang)) {
    console.warn("targetLang was an array, converting to string");
    targetLang = targetLang[0];
  }
  
  const key = generateKey(text, context);
  const cacheKey = `${targetLang}:${key}`;
  const startTime = global.translationStartTime || Date.now();
  
  // Add to memory cache
  addToMemoryCache(cacheKey, translatedText);
  
  // Save to database if available
  if (TranslationCache) {
    try {
      console.log(`Saving translation to cache for language: ${targetLang}`);
      const [record, created] = await TranslationCache.findOrCreate({
        where: {
          key,
          language: targetLang
        },
        defaults: {
          originalText: text.substring(0, 500), // Limit text size
          translatedText,
          context: context || null,
          source: 'openai',
          charCount: text.length,
          // Add metrics for analysis
          processingTime: Date.now() - startTime,
          apiVersion: 'gpt-4o-mini'
        }
      });
      
      if (!created) {
        // Update existing record
        await record.update({
          translatedText,
          lastUsed: new Date(),
          useCount: record.useCount + 1
        });
      }
    } catch (error) {
      console.error('Error saving to database cache:', error);
    }
  }
}

/**
 * Check if text contains HTML tags
 * @param {string} text - Text to check
 * @returns {boolean} - true if contains tags
 */
function containsHtmlTags(text) {
  return /<[^>]*>/g.test(text);
}

/**
 * Translate text using OpenAI
 * @param {string|string[]} text - Source text
 * @param {string|string[]} targetLang - Target language
 * @param {string} context - Translation context
 * @param {string|string[]} sourceLang - Source language (default: en)
 * @returns {Promise<string>} - Translated text
 */
async function translateText(text, targetLang, context = '', sourceLang = 'en') {
  try {
    // Ensure language parameters are strings, not arrays
    const targetLanguage = Array.isArray(targetLang) ? targetLang[0] : targetLang;
    const sourceLanguage = Array.isArray(sourceLang) ? sourceLang[0] : sourceLang;
    
    // If text is empty, return as is
    if (!text || text.trim() === '') {
      return text;
    }
    
    console.log(`Translating: "${text.substring(0, 30)}..." to ${targetLanguage}`);
    
    // Check cache before API call
    const cachedTranslation = await checkCache(text, targetLanguage, context);
    if (cachedTranslation) return cachedTranslation;
    
    // Get full language name for more accurate translation
    const languageNames = {
      'de': 'German', 'en': 'English', 'fr': 'French', 'it': 'Italian',
      'es': 'Spanish', 'pt': 'Portuguese', 'nl': 'Dutch', 'da': 'Danish',
      'sv': 'Swedish', 'fi': 'Finnish', 'el': 'Greek', 'cs': 'Czech',
      'pl': 'Polish', 'hu': 'Hungarian', 'sk': 'Slovak', 'sl': 'Slovenian',
      'et': 'Estonian', 'lv': 'Latvian', 'lt': 'Lithuanian', 'ro': 'Romanian',
      'bg': 'Bulgarian', 'hr': 'Croatian', 'ga': 'Irish', 'mt': 'Maltese',
      'ru': 'Russian', 'tr': 'Turkish', 'ar': 'Arabic', 'zh': 'Chinese',
      'uk': 'Ukrainian', 'sr': 'Serbian', 'he': 'Hebrew', 'ko': 'Korean', 
      'ja': 'Japanese'
    };
    
    const targetLanguageName = languageNames[targetLanguage] || targetLanguage;
    const sourceLanguageName = languageNames[sourceLanguage] || sourceLanguage;
    
    // Create system prompt with context
    let systemPrompt = `You are a professional translator for a warehouse management system. 
Translate the text from ${sourceLanguageName} to ${targetLanguageName}. 
Maintain the same tone, formatting and technical terminology.
Ensure the translation sounds natural in the target language.`;
    
    // Add instructions for preserving HTML tags if present
    if (containsHtmlTags(text)) {
      systemPrompt += `\nIMPORTANT: Preserve all HTML tags exactly as they appear in the original text.`;
    }
    
    if (context) {
      systemPrompt += `\nContext: This text appears in the "${context}" section of the application.`;
    }
    
    // Special instructions for specific languages
    if (targetLanguage === 'ar' || targetLanguage === 'he') {
      systemPrompt += '\nNote: This language is read from right to left.';
    } else if (targetLanguage === 'zh') {
      systemPrompt += '\nUse Simplified Chinese characters.';
    } else if (targetLanguage === 'ja' || targetLanguage === 'ko') {
      systemPrompt += '\nPreserve technical terms in their standard form for this language.';
    }
    
    console.log(`Calling OpenAI API for translation to ${targetLanguage}`);
    
    // Call OpenAI API
    const response = await openai.chat.completions.create({
      model: "gpt-4o-mini", // or another available model
      messages: [
        { role: "system", content: systemPrompt },
        { role: "user", content: text }
      ],
      temperature: 0.3, // Low temperature for more accurate translations
      max_tokens: Math.max(text.length * 2, 500) // Dynamic token limit
    });
    
    const translatedText = response.choices[0].message.content.trim();
    
    // Save to cache - with proper string language parameter
    await saveToCache(text, targetLanguage, translatedText, context);
    
    return translatedText;
  } catch (error) {
    console.error(`Translation error for language ${Array.isArray(targetLang) ? targetLang[0] : targetLang}:`, error);
    // In case of error, return original text
    return text;
  }
}

/**
 * Batch translation function
 * @param {Array<string>} texts - Array of texts to translate
 * @param {string|string[]} targetLang - Target language
 * @param {string} context - Translation context
 * @param {string|string[]} sourceLang - Source language (default: en)
 * @returns {Promise<Array<string>>} - Array of translated texts
 */
async function batchTranslate(texts, targetLang, context = '', sourceLang = 'en') {
  // Ensure language parameters are strings, not arrays
  const targetLanguage = Array.isArray(targetLang) ? targetLang[0] : targetLang;
  const sourceLanguage = Array.isArray(sourceLang) ? sourceLang[0] : sourceLang;
  
  console.log(`Batch translating ${texts.length} texts to ${targetLanguage}`);
  
  // Check if all texts are in cache
  const results = [];
  const missingTexts = [];
  const missingIndexes = [];
  
  // First check if texts are in cache
  for (let i = 0; i < texts.length; i++) {
    if (!texts[i] || texts[i].trim() === '') {
      results[i] = texts[i];
      continue;
    }
    
    // Check if the text is a tagged key
    const isTaggedKey = /^data-i18n=["'][^"']+["']$/.test(texts[i]);
    
    if (isTaggedKey) {
      // Keep tag as is for later frontend processing
      results[i] = texts[i];
      continue;
    }
    
    // Use the single language string when checking cache
    const cachedText = await checkCache(texts[i], targetLanguage, context);
    if (cachedText) {
      results[i] = cachedText;
    } else {
      missingTexts.push(texts[i]);
      missingIndexes.push(i);
    }
  }
  
  // If there are missing translations, translate them as a batch
  if (missingTexts.length > 0) {
    // Split into batches of 20 texts for API optimization
    const BATCH_SIZE = 20;
    
    for (let batchStart = 0; batchStart < missingTexts.length; batchStart += BATCH_SIZE) {
      const batchEnd = Math.min(batchStart + BATCH_SIZE, missingTexts.length);
      const currentBatch = missingTexts.slice(batchStart, batchEnd);
      const currentIndexes = missingIndexes.slice(batchStart, batchEnd);
      
      const combinedText = currentBatch.join("\n---SEPARATOR---\n");
      
      // Get full language names
      const languageNames = {
        'de': 'German', 'en': 'English', 'fr': 'French', 'it': 'Italian',
        'es': 'Spanish', 'pt': 'Portuguese', 'nl': 'Dutch', 'da': 'Danish',
        'sv': 'Swedish', 'fi': 'Finnish', 'el': 'Greek', 'cs': 'Czech',
        'pl': 'Polish', 'hu': 'Hungarian', 'sk': 'Slovak', 'sl': 'Slovenian',
        'et': 'Estonian', 'lv': 'Latvian', 'lt': 'Lithuanian', 'ro': 'Romanian',
        'bg': 'Bulgarian', 'hr': 'Croatian', 'ga': 'Irish', 'mt': 'Maltese',
        'ru': 'Russian', 'tr': 'Turkish', 'ar': 'Arabic', 'zh': 'Chinese',
        'uk': 'Ukrainian', 'sr': 'Serbian', 'he': 'Hebrew', 'ko': 'Korean', 
        'ja': 'Japanese'
      };
      
      // Use the string language values for names
      const targetLanguageName = languageNames[targetLanguage] || targetLanguage;
      const sourceLanguageName = languageNames[sourceLanguage] || sourceLanguage;
      
      const systemPrompt = `You are a professional translator for a warehouse management system.
Translate the following texts from ${sourceLanguageName} to ${targetLanguageName}.
Each text is separated by "---SEPARATOR---".
Maintain the same tone, formatting and technical terminology.
Ensure the translations sound natural in ${targetLanguageName}.
Return ONLY the translated texts, with each separated by "---SEPARATOR---".
Keep the same order of texts.

IMPORTANT: If a text contains HTML tags, preserve them exactly as they appear in the original text.`;
      
      console.log(`OpenAI API batch call for ${currentBatch.length} texts to ${targetLanguage}`);
      
      try {
        const response = await openai.chat.completions.create({
          model: "gpt-4o-mini", // or another available model
          messages: [
            { role: "system", content: systemPrompt },
            { role: "user", content: combinedText }
          ],
          temperature: 0.3
        });
        
        const translatedContent = response.choices[0].message.content.trim();
        const translatedTexts = translatedContent.split("---SEPARATOR---").map(t => t.trim());
        
        // Check if number of translated texts matches source
        if (translatedTexts.length === currentBatch.length) {
          // Save translations to cache and fill results
          for (let i = 0; i < currentBatch.length; i++) {
            const originalText = currentBatch[i];
            const translatedText = translatedTexts[i];
            
            // Save to cache - with proper string language parameter
            await saveToCache(originalText, targetLanguage, translatedText, context);
            
            // Fill results
            results[currentIndexes[i]] = translatedText;
          }
        } else {
          // If mismatch, translate individually
          for (let i = 0; i < currentBatch.length; i++) {
            // Pass the string language to translateText
            const translatedText = await translateText(
              currentBatch[i], 
              targetLanguage,  // Use string language
              context, 
              sourceLanguage   // Use string language
            );
            results[currentIndexes[i]] = translatedText;
          }
        }
      } catch (error) {
        console.error("Batch translation error:", error);
        
        // In case of error, translate individually
        for (let i = 0; i < currentBatch.length; i++) {
          // Pass the string language to translateText
          const translatedText = await translateText(
            currentBatch[i], 
            targetLanguage,  // Use string language
            context, 
            sourceLanguage   // Use string language
          );
          results[currentIndexes[i]] = translatedText;
        }
      }
    }
  }
  
  return results;
}

module.exports = {
  translateText,
  batchTranslate,
  checkCache,
  saveToCache
};