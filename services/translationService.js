// services/translationService.js - Updated for i18next compatibility

const { OpenAI } = require('openai');
const crypto = require('crypto');
const path = require('path');
const { stripBOM } = require('../utils/bomUtils');

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
  console.warn('PostgreSQL not configured for translation cache. Using memory cache only.');
}

// In-memory cache for faster lookup of recent translations
const memoryCache = new Map();
const MEMORY_CACHE_MAX_SIZE = 1000; // Limit memory cache size

/**
 * Generate consistent key for caching
 * @param {string} text - Source text
 * @param {string} context - Translation context
 * @returns {string} - MD5 hash
 */
function generateKey(text, context = '') {
  // Ensure BOM is stripped before generating the hash key
  const cleanText = stripBOM(text);
  return crypto
    .createHash('md5')
    .update(`${cleanText}_${context}`)
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
  try {
    // Strip BOM before generating key
    text = stripBOM(text);
    
    // Generate consistent key
    const key = generateKey(text, context);
    const cacheKey = `${targetLang}:${key}`;
    
    // Add detailed logging in development
    if (process.env.NODE_ENV === 'development') {
      console.log(`[i18n] Checking cache for: ${cacheKey.substring(0, 20)}... (text: "${text.substring(0, 30)}...")`);
    }
    
    // Check memory cache first for fastest access
    if (memoryCache.has(cacheKey)) {
      if (process.env.NODE_ENV === 'development') {
        console.log(`[i18n] Memory cache hit for: ${cacheKey.substring(0, 20)}...`);
      }
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
          // Update usage metrics asynchronously (don't wait for this)
          cachedTranslation.update({
            lastUsed: new Date(),
            useCount: cachedTranslation.useCount + 1
          }).catch(err => {
            console.error('Error updating cache metrics:', err);
          });
          
          // Add to memory cache for faster future access
          addToMemoryCache(cacheKey, cachedTranslation.translatedText);
          
          if (process.env.NODE_ENV === 'development') {
            console.log(`[i18n] Database cache hit for: ${cacheKey.substring(0, 20)}...`);
          }
          
          return cachedTranslation.translatedText;
        }
      } catch (error) {
        console.error('Error checking database cache:', error);
      }
    }
    
    if (process.env.NODE_ENV === 'development') {
      console.log(`[i18n] Cache miss for: ${cacheKey.substring(0, 20)}...`);
    }
    
    return null;
  } catch (error) {
    console.error('Cache check error:', error);
    return null; // On error, return null to indicate cache miss
  }
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
 * Save translation to memory cache and database
 * @param {string} text - Source text
 * @param {string} targetLang - Target language (must be a string)
 * @param {string} translatedText - Translated text
 * @param {string} context - Translation context
 */
async function saveToCache(text, targetLang, translatedText, context = '') {
  try {
    // Make sure targetLang is a string
    if (Array.isArray(targetLang)) {
      console.warn("targetLang was an array, converting to string");
      targetLang = targetLang[0];
    }
    
    // Skip saving if text and translation are identical
    if (text === translatedText) {
      return;
    }
    
    // Strip BOM before generating key
    text = stripBOM(text);
    const key = generateKey(text, context);
    const cacheKey = `${targetLang}:${key}`;
    const startTime = global.translationStartTime || Date.now();
    
    // Add to memory cache
    addToMemoryCache(cacheKey, translatedText);
    
    // Save to database if available
    if (TranslationCache) {
      try {
        if (process.env.NODE_ENV === 'development') {
          console.log(`[i18n] Saving translation to cache: ${cacheKey.substring(0, 20)}...`);
        }
        
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
          // Update existing record if translation has changed
          if (record.translatedText !== translatedText) {
            await record.update({
              translatedText,
              lastUsed: new Date(),
              useCount: record.useCount + 1
            });
          } else {
            // Just update usage metrics if translation is the same
            await record.update({
              lastUsed: new Date(),
              useCount: record.useCount + 1
            });
          }
        }
      } catch (error) {
        console.error('Error saving to database cache:', error);
      }
    }
  } catch (error) {
    console.error('Error in saveToCache:', error);
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
 * Translate text using OpenAI - primary translation function
 * @param {string} text - Source text
 * @param {string} targetLang - Target language
 * @param {string} context - Translation context
 * @param {string} sourceLang - Source language (default: en)
 * @param {object} options - Additional options like count for placeholders
 * @returns {Promise<string>} - Translated text
 */
async function translateText(text, targetLang, context = '', sourceLang = process.env.DEFAULT_LANGUAGE || 'en', options = {}) {
  try {
    // Track performance
    global.translationStartTime = Date.now();
    
    // Ensure language parameters are strings
    const targetLanguage = Array.isArray(targetLang) ? targetLang[0] : targetLang;
    const sourceLanguage = Array.isArray(sourceLang) ? sourceLang[0] : sourceLang;
    
    // Strip BOM if present
    text = stripBOM(text);
    
    // If text is empty, return as is
    if (!text || text.trim() === '') {
      return text;
    }
    
    console.log(`Translating: "${text.substring(0, 30)}..." to ${targetLanguage}`);
    
    // Create a unique cache key that includes relevant options (like count)
    let optionsCacheKey = '';
    if (options.count !== undefined) {
      optionsCacheKey = `:count=${options.count}`;
    }
    
    // Check cache before API call with extended cache key
    const cachedTranslation = await checkCache(text + optionsCacheKey, targetLanguage, context);
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
    
    // Get domain description from environment variable
    const domainDescription = process.env.TRANSLATION_DOMAIN || 'for a warehouse management system';
    
    // Create system prompt based on whether it's a translation or grammar correction
    let systemPrompt;
    
    if (sourceLanguage === targetLanguage) {
      // Grammar correction prompt when languages are the same
      systemPrompt = `You are a professional editor and proofreader ${domainDescription}. 
Review and correct the text in ${targetLanguageName}.
Fix any grammatical errors, improve clarity and consistency, and ensure proper terminology.
Maintain the same overall meaning and style.
Return the corrected text without explaining the changes.`;
    } else {
      // Translation prompt when languages are different
      systemPrompt = `You are a professional translator ${domainDescription}. 
Translate the text from ${sourceLanguageName} to ${targetLanguageName}. 
Maintain the same tone, formatting and technical terminology.
Ensure the translation sounds natural in the target language.`;
    }
    
    // Add instructions for preserving HTML tags if present
    if (containsHtmlTags(text)) {
      systemPrompt += `\nIMPORTANT: Preserve all HTML tags exactly as they appear in the original text.`;
    }
    
    if (context) {
      systemPrompt += `\nContext: This text appears in the "${context}" section of the application.`;
    }
    
    // НОВОЕ: Добавление инструкций по обработке счетчиков и других опций
    if (options.count !== undefined) {
      systemPrompt += `\nIMPORTANT: The text may contain the placeholder {{count}} which should be replaced with the number ${options.count} while translating.`;
    }
    
    // Special instructions for specific languages
    if (targetLanguage === 'ar' || targetLanguage === 'he') {
      systemPrompt += '\nNote: This language is read from right to left.';
    } else if (targetLanguage === 'zh') {
      systemPrompt += '\nUse Simplified Chinese characters.';
    } else if (targetLanguage === 'ja' || targetLanguage === 'ko') {
      systemPrompt += '\nPreserve technical terms in their standard form for this language.';
    }
    
    // Call OpenAI API
    const response = await openai.chat.completions.create({
      model: "gpt-4o-mini", // or another available model
      messages: [
        { role: "system", content: systemPrompt },
        { role: "user", content: text }
      ],
      temperature: 0.3, // Low temperature for more accurate translations/corrections
      max_tokens: Math.max(text.length * 2, 500) // Dynamic token limit
    });
    
    let translatedText = response.choices[0].message.content.trim();
    
    // Strip BOM from translated text if present
    translatedText = stripBOM(translatedText);
    
    // НОВОЕ: Заменяем {{count}} на options.count напрямую для более надежной работы
    if (options.count !== undefined && translatedText.includes('{{count}}')) {
      translatedText = translatedText.replace(/\{\{count\}\}/g, options.count);
    }
    
    // Save to cache with extended key
    await saveToCache(text + optionsCacheKey, targetLanguage, translatedText, context);
    
    return translatedText;
  } catch (error) {
    console.error(`Translation error for language ${Array.isArray(targetLang) ? targetLang[0] : targetLang}:`, error);
    // In case of error, return original text
    return text;
  }
}

/**
 * Batch translate multiple texts
 * @param {string[]} texts - Array of texts to translate
 * @param {string} targetLang - Target language
 * @param {string} context - Translation context (usually namespace)
 * @param {string} sourceLang - Source language (default: en)
 * @param {object|object[]} options - Options for each text (single object or array of objects)
 * @returns {Promise<string[]>} - Array of translated texts
 */
async function batchTranslate(texts, targetLang, context = '', sourceLang = process.env.DEFAULT_LANGUAGE || 'en', options = []) {
  // Ensure language parameters are strings
  const targetLanguage = Array.isArray(targetLang) ? targetLang[0] : targetLang;
  const sourceLanguage = Array.isArray(sourceLang) ? sourceLang[0] : sourceLang;
  
  // Normalize options to array
  const optionsArray = Array.isArray(options) ? options : Array(texts.length).fill(options);
  
  if (!texts || !texts.length) {
    return [];
  }
  
  // Detect if this is a grammar correction (same language) or translation
  const isGrammarCorrection = sourceLanguage === targetLanguage;
  
  console.log(`Batch ${isGrammarCorrection ? 'grammar correction' : 'translation'} for ${texts.length} texts in ${targetLanguage}`);
  
  // Record start time for performance tracking
  const startTime = Date.now();
  
  // First check what's already in cache
  const results = new Array(texts.length);
  const missingTexts = [];
  const missingOptions = [];
  const missingIndexes = [];
  
  // First pass: Check cache for each text
  for (let i = 0; i < texts.length; i++) {
    const text = texts[i]?.trim();
    const textOptions = optionsArray[i] || {};
    
    // Skip empty texts
    if (!text) {
      results[i] = text;
      continue;
    }
    
    // Create extended cache key with options
    let optionsCacheKey = '';
    if (textOptions.count !== undefined) {
      optionsCacheKey = `:count=${textOptions.count}`;
    }
    
    // Try cache first with extended key
    const cachedText = await checkCache(text + optionsCacheKey, targetLanguage, context);
    if (cachedText) {
      results[i] = cachedText;
    } else {
      missingTexts.push(text);
      missingOptions.push(textOptions);
      missingIndexes.push(i);
    }
  }
  
  // If we found everything in cache, return immediately
  if (missingTexts.length === 0) {
    console.log(`All ${texts.length} texts found in cache!`);
    return results;
  }
  
  // Only send uncached texts to the API, in batches of 20 
  const BATCH_SIZE = 20;
  const apiCalls = [];
  
  for (let i = 0; i < missingTexts.length; i += BATCH_SIZE) {
    const batch = missingTexts.slice(i, i + BATCH_SIZE);
    const batchOptions = missingOptions.slice(i, i + BATCH_SIZE);
    const batchIndexes = missingIndexes.slice(i, i + BATCH_SIZE);
    
    // Join with specific separator for reliable splitting
    const combinedText = batch.join("\n---TRANSLATION_SEPARATOR---\n");
    
    // Get language names for better translation quality
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
    
    // Domain context from environment
    const domainDescription = process.env.TRANSLATION_DOMAIN || 'for a warehouse management system';
    
    // Create a specialized prompt based on whether this is grammar correction or translation
    let systemPrompt;
    
    if (isGrammarCorrection) {
      // Grammar correction prompt
      systemPrompt = `You are a professional editor and proofreader ${domainDescription}.
Review and correct the following texts in ${targetLanguageName}.
Each text is separated by "---TRANSLATION_SEPARATOR---".
Fix any grammatical errors, improve clarity and consistency, and ensure proper terminology.
Maintain the same overall meaning and style for each text.
Return ONLY the corrected texts, with each separated by "---TRANSLATION_SEPARATOR---".
Keep the exact same order of texts.

IMPORTANT: If a text contains HTML tags, preserve them exactly as they appear in the original text.
IMPORTANT: Provide EXACTLY the same number of texts as in the input, separated by "---TRANSLATION_SEPARATOR---".`;
    } else {
      // Translation prompt
      systemPrompt = `You are a professional translator ${domainDescription}.
Translate the following texts from ${sourceLanguageName} to ${targetLanguageName}.
Each text is separated by "---TRANSLATION_SEPARATOR---".
Maintain the same tone, formatting, and technical terminology.
Ensure the translations sound natural in ${targetLanguageName}.
Return ONLY the translated texts, with each separated by "---TRANSLATION_SEPARATOR---".
Keep the exact same order of texts.

IMPORTANT: If a text contains HTML tags, preserve them exactly as they appear in the original text.
IMPORTANT: Provide EXACTLY the same number of texts as in the input, separated by "---TRANSLATION_SEPARATOR---".`;
    }
    
    // Add context if provided
    if (context) {
      systemPrompt += `\nContext: These texts appear in the "${context}" section of the application.`;
    }
    
    // НОВОЕ: Информация о placeholder'ах с учетом того, что разные тексты могут иметь разные count
    let hasCountPlaceholders = false;
    const countsInfo = [];
    
    batchOptions.forEach((textOptions, idx) => {
      if (textOptions.count !== undefined) {
        hasCountPlaceholders = true;
        countsInfo.push(`Text ${idx+1}: replace {{count}} with ${textOptions.count}`);
      }
    });
    
    if (hasCountPlaceholders) {
      systemPrompt += `\nIMPORTANT: Some texts contain the placeholder {{count}} which should be replaced with specific numbers:
${countsInfo.join('\n')}`;
    }
    
    // Special instructions for specific languages
    if (targetLanguage === 'ar' || targetLanguage === 'he') {
      systemPrompt += '\nNote: This language is read from right to left.';
    } else if (targetLanguage === 'zh') {
      systemPrompt += '\nUse Simplified Chinese characters.';
    } else if (targetLanguage === 'ja' || targetLanguage === 'ko') {
      systemPrompt += '\nPreserve technical terms in their standard form for this language.';
    }
    
    apiCalls.push(async () => {
      try {
        console.log(`Calling OpenAI API for batch ${isGrammarCorrection ? 'grammar correction' : 'translation'} of ${batch.length} texts (${targetLanguage})`);
        
        const response = await openai.chat.completions.create({
          model: "gpt-4o-mini",
          messages: [
            { role: "system", content: systemPrompt },
            { role: "user", content: combinedText }
          ],
          temperature: 0.3 // Low temperature for more accurate translations/corrections
        });
        
        // Parse response and handle the translated texts
        const translatedContent = stripBOM(response.choices[0].message.content.trim());
        const translatedTexts = translatedContent.split("---TRANSLATION_SEPARATOR---").map(t => stripBOM(t.trim()));
        
        // Validate that we got the right number of translations
        if (translatedTexts.length !== batch.length) {
          console.error(`${isGrammarCorrection ? 'Correction' : 'Translation'} count mismatch! Expected ${batch.length}, got ${translatedTexts.length}`);
          // If count doesn't match, translate individually as fallback
          for (let j = 0; j < batch.length; j++) {
            const singleText = await translateText(
              batch[j], 
              targetLanguage, 
              context, 
              sourceLanguage, 
              batchOptions[j] || {}
            );
            results[batchIndexes[j]] = singleText;
          }
        } else {
          // Save each translation to cache and results array
          for (let j = 0; j < translatedTexts.length; j++) {
            const originalText = batch[j];
            let translatedText = translatedTexts[j];
            const textOptions = batchOptions[j] || {};
            
            // Дополнительная обработка для {{count}}
            if (textOptions.count !== undefined && translatedText.includes('{{count}}')) {
              translatedText = translatedText.replace(/\{\{count\}\}/g, textOptions.count);
            }
            
            // Create extended cache key with options
            let optionsCacheKey = '';
            if (textOptions.count !== undefined) {
              optionsCacheKey = `:count=${textOptions.count}`;
            }
            
            // Save to cache with extended key
            await saveToCache(originalText + optionsCacheKey, targetLanguage, translatedText, context);
            
            // Store in results array
            results[batchIndexes[j]] = translatedText;
          }
        }
      } catch (error) {
        console.error(`Batch API error (${isGrammarCorrection ? 'grammar correction' : 'translation'}):`, error);
        
        // Fall back to individual translations
        for (let j = 0; j < batch.length; j++) {
          // Use direct translateText call with options
          const singleText = await translateText(
            batch[j], 
            targetLanguage, 
            context, 
            sourceLanguage,
            batchOptions[j] || {}
          );
          results[batchIndexes[j]] = singleText;
        }
      }
    });
  }
  
  // Execute API calls in parallel (limit concurrency to avoid rate limits)
  const concurrencyLimit = 2; // Max 2 concurrent API calls
  for (let i = 0; i < apiCalls.length; i += concurrencyLimit) {
    const batchCalls = apiCalls.slice(i, i + concurrencyLimit);
    await Promise.all(batchCalls.map(call => call()));
  }
  
  // Log performance metrics
  const totalTime = Date.now() - startTime;
  const timePerText = totalTime / texts.length;
  console.log(`Batch ${isGrammarCorrection ? 'grammar correction' : 'translation'} complete: ${texts.length} texts in ${totalTime}ms (avg ${timePerText.toFixed(2)}ms per text)`);
  
  return results;
}

module.exports = {
  translateText,
  batchTranslate,
  checkCache,
  saveToCache
};