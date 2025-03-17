// services/translationService.js
const { OpenAI } = require('openai');
const crypto = require('crypto');
const path = require('path');
const fs = require('fs');

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

// Directory for file-based translation cache
const CACHE_DIR = path.join(process.cwd(), 'cache', 'translations');

// Create cache directory if it doesn't exist
if (!fs.existsSync(CACHE_DIR)) {
  fs.mkdirSync(CACHE_DIR, { recursive: true });
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
 * Check translation cache
 * @param {string} text - Source text
 * @param {string} targetLang - Target language
 * @param {string} context - Translation context
 * @returns {Promise<string|null>} - Translation or null if not found
 */
async function checkCache(text, targetLang, context = '') {
  const key = generateKey(text, context);
  const cacheKey = `${targetLang}:${key}`;
  
  // First check memory cache for fastest response
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
        // Update usage count and date
        await cachedTranslation.update({
          lastUsed: new Date(),
          useCount: cachedTranslation.useCount + 1
        });
        
        // Add to memory cache
        addToMemoryCache(cacheKey, cachedTranslation.translatedText);
        
        return cachedTranslation.translatedText;
      }
    } catch (error) {
      console.error('Error checking database cache:', error);
    }
  }
  
  // Finally check file cache
  const cacheFile = path.join(CACHE_DIR, `${targetLang}_${key}.json`);
  if (fs.existsSync(cacheFile)) {
    try {
      const cacheData = JSON.parse(fs.readFileSync(cacheFile, 'utf8'));
      
      // Add to memory cache
      addToMemoryCache(cacheKey, cacheData.translatedText);
      
      return cacheData.translatedText;
    } catch (error) {
      console.error('Error reading file cache:', error);
    }
  }
  
  return null;
}

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
 * Save translation to cache
 * @param {string} text - Source text
 * @param {string} targetLang - Target language
 * @param {string} translatedText - Translated text
 * @param {string} context - Translation context
 */
async function saveToCache(text, targetLang, translatedText, context = '') {
  const key = generateKey(text, context);
  const cacheKey = `${targetLang}:${key}`;
  
  // Add to memory cache
  addToMemoryCache(cacheKey, translatedText);
  
  // Save to database if available
  if (TranslationCache) {
    try {
      await TranslationCache.findOrCreate({
        where: {
          key,
          language: targetLang
        },
        defaults: {
          originalText: text,
          translatedText: translatedText,
          context: context || null
        }
      }).then(([record, created]) => {
        if (!created) {
          // Update existing record
          return record.update({
            translatedText: translatedText,
            lastUsed: new Date(),
            useCount: record.useCount + 1
          });
        }
      });
    } catch (error) {
      console.error('Error saving to database cache:', error);
    }
  }
  
  // Always save to file cache as backup
  const cacheFile = path.join(CACHE_DIR, `${targetLang}_${key}.json`);
  const cacheData = {
    key,
    language: targetLang,
    originalText: text,
    translatedText: translatedText,
    context: context || null,
    timestamp: new Date().toISOString()
  };
  
  try {
    fs.writeFileSync(cacheFile, JSON.stringify(cacheData, null, 2), 'utf8');
  } catch (error) {
    console.error('Error saving to file cache:', error);
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
 * @param {string} text - Source text
 * @param {string} targetLang - Target language
 * @param {string} context - Translation context
 * @param {string} sourceLang - Source language (default: de)
 * @returns {Promise<string>} - Translated text
 */
async function translateText(text, targetLang, context = '', sourceLang = 'de') {
  try {
    // If text is empty, return as is
    if (!text || text.trim() === '') {
      return text;
    }
    
    // Check cache before API call
    const cachedTranslation = await checkCache(text, targetLang, context);
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
    
    const targetLanguageName = languageNames[targetLang] || targetLang;
    const sourceLanguageName = languageNames[sourceLang] || sourceLang;
    
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
    if (targetLang === 'ar' || targetLang === 'he') {
      systemPrompt += '\nNote: This language is read from right to left.';
    } else if (targetLang === 'zh') {
      systemPrompt += '\nUse Simplified Chinese characters.';
    } else if (targetLang === 'ja' || targetLang === 'ko') {
      systemPrompt += '\nPreserve technical terms in their standard form for this language.';
    }
    
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
    
    // Save to cache
    await saveToCache(text, targetLang, translatedText, context);
    
    return translatedText;
  } catch (error) {
    console.error("Translation error:", error);
    // In case of error, return original text
    return text;
  }
}

/**
 * Batch translation function
 * @param {Array<string>} texts - Array of texts to translate
 * @param {string} targetLang - Target language
 * @param {string} context - Translation context
 * @param {string} sourceLang - Source language (default: de)
 * @returns {Promise<Array<string>>} - Array of translated texts
 */
async function batchTranslate(texts, targetLang, context = '', sourceLang = 'de') {
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
    
    const cachedText = await checkCache(texts[i], targetLang, context);
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
      
      const targetLanguageName = languageNames[targetLang] || targetLang;
      const sourceLanguageName = languageNames[sourceLang] || sourceLang;
      
      const systemPrompt = `You are a professional translator for a warehouse management system.
Translate the following texts from ${sourceLanguageName} to ${targetLanguageName}.
Each text is separated by "---SEPARATOR---".
Maintain the same tone, formatting and technical terminology.
Ensure the translations sound natural in ${targetLanguageName}.
Return ONLY the translated texts, with each separated by "---SEPARATOR---".
Keep the same order of texts.

IMPORTANT: If a text contains HTML tags, preserve them exactly as they appear in the original text.`;
      
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
            
            // Save to cache
            await saveToCache(originalText, targetLang, translatedText, context);
            
            // Fill results
            results[currentIndexes[i]] = translatedText;
          }
        } else {
          // If mismatch, translate individually
          for (let i = 0; i < currentBatch.length; i++) {
            const translatedText = await translateText(currentBatch[i], targetLang, context, sourceLang);
            results[currentIndexes[i]] = translatedText;
          }
        }
      } catch (error) {
        console.error("Batch translation error:", error);
        
        // In case of error, translate individually
        for (let i = 0; i < currentBatch.length; i++) {
          const translatedText = await translateText(currentBatch[i], targetLang, context, sourceLang);
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