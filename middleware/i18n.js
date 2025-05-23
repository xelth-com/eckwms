// middleware/i18n.js - Fix for sourceType reference error
const i18next = require('i18next');
const i18nextMiddleware = require('i18next-http-middleware');
const Backend = require('i18next-fs-backend');
const path = require('path');
const { translateText, saveToCache, batchTranslate } = require('../services/translationService');

const { Queue } = require('../utils/queue');
const fs = require('fs');
const { stripBOM, parseJSONWithBOM } = require('../utils/bomUtils');


const namespaceVersions = new Map();



// middleware/i18n.js - Optimized missing key handler

// Initialize translation queue for handling missing keys
const translationQueue = new Queue();
// Track keys currently being processed to prevent recursion
const processingKeys = new Set();
// Cache of already requested translations to avoid duplicates
const requestedTranslations = new Map();
// Cleanup interval for requested translations (5 minutes)
setInterval(() => {
  const now = Date.now();
  let cleaned = 0;

  requestedTranslations.forEach((timestamp, key) => {
    if (now - timestamp > 300000) { // 5 minutes
      requestedTranslations.delete(key);
      cleaned++;
    }
  });

  if (cleaned > 0) {
    console.log(`[i18n] Cleaned up ${cleaned} stale translation requests`);
  }
}, 300000);

/**
 * Generate consistent key for tracking translation requests
 * @param {string} text - Source text
 * @param {string} targetLang - Target language
 * @param {string} namespace - Translation namespace
 * @returns {string} - Unique key
 */
function generateTrackingKey(text, targetLang, namespace) {
  // Using MD5 would be better, but for tracking we can use a simpler approach
  // This is just for in-memory tracking, not for database storage
  return `${targetLang}:${namespace}:${text.substring(0, 50)}`;
}

/**
 * Check if a translation has already been requested recently
 * @param {string} text - Source text 
 * @param {string} targetLang - Target language
 * @param {string} namespace - Translation namespace
 * @returns {boolean} - True if already requested
 */
function isAlreadyRequested(text, targetLang, namespace) {
  const key = generateTrackingKey(text, targetLang, namespace);
  return requestedTranslations.has(key);
}

/**
 * Mark a translation as requested
 * @param {string} text - Source text
 * @param {string} targetLang - Target language
 * @param {string} namespace - Translation namespace
 */
function markAsRequested(text, targetLang, namespace) {
  const key = generateTrackingKey(text, targetLang, namespace);
  requestedTranslations.set(key, Date.now());
}

/**
 * Optimized missing key handler for i18next
 * @param {string|string[]} lng - Target language(s)
 * @param {string} ns - Namespace
 * @param {string} key - Translation key
 * @param {string} fallbackValue - Default value if missing
 * @param {object} options - i18next options
 * @param {object} req - Express request object (optional)
 */
function optimizedMissingKeyHandler(lng, ns, key, fallbackValue, options, req) {
  // Get the primary language from the array or use the language if it's already a string
  const targetLanguage = Array.isArray(lng) ? lng[0] : lng;
  const defaultLanguage = process.env.DEFAULT_LANGUAGE || 'en';

  // Make sure we have a valid target language that's not the default
  if (!targetLanguage || targetLanguage === defaultLanguage) {
    return;  // Skip processing for default language
  }

  // If no fallback value provided, just use the key
  const textToTranslate = fallbackValue || key;

  // Skip if already being processed or empty
  if (!textToTranslate || textToTranslate.trim() === '') {
    return;
  }

  // Create a unique key to check for duplicates
  const uniqueKey = `${targetLanguage}:${ns}:${key}`;

  // Skip if this key is already being processed
  if (processingKeys.has(uniqueKey)) {
    return;
  }

  // Skip if this translation was already requested recently
  if (isAlreadyRequested(textToTranslate, targetLanguage, ns)) {
    return;
  }

  try {
    // Mark that we're processing this key
    processingKeys.add(uniqueKey);

    // Mark this translation as requested to prevent duplicates
    markAsRequested(textToTranslate, targetLanguage, ns);

    // First check if it already exists in the cache
    // This avoids adding duplicates to the queue
    checkCache(textToTranslate, targetLanguage, ns)
      .then(cachedResult => {
        if (cachedResult) {
          // Already in cache, no need to queue translation
          console.log(`[i18n] Missing key ${uniqueKey} found in cache, skipping translation`);
          return;
        }

        // Log only if not in cache
        console.log(`[i18n] Missing translation: [${targetLanguage}] ${ns}:${key}`);

        // Add to queue with relevant info
        translationQueue.enqueue({
          text: textToTranslate,
          targetLang: targetLanguage,
          namespace: ns,
          key: key,
          priority: 100 // High priority for missing keys
        });
      })
      .catch(error => {
        console.error(`[i18n] Error checking cache for missing key:`, error);
        // On error, add to queue anyway
        translationQueue.enqueue({
          text: textToTranslate,
          targetLang: targetLanguage,
          namespace: ns,
          key: key
        });
      })
      .finally(() => {
        // Always remove from processing list when done
        processingKeys.delete(uniqueKey);
      });
  } catch (error) {
    // Handle any synchronous errors
    console.error(`[i18n] Error in missing key handler:`, error);
    processingKeys.delete(uniqueKey);
  }
}

/**
 * Load namespace with version tracking
 * @param {string} language - Language code
 * @param {string} namespace - Namespace to load
 * @param {string} version - Optional version identifier
 * @returns {Promise<void>}
 */
async function loadNamespace(language, namespace, version = '') {
  const cacheKey = `${language}:${namespace}`;
  const currentVersion = namespaceVersions.get(cacheKey) || '';

  // Skip if already loaded with current version
  if (loadedNamespaces && loadedNamespaces[cacheKey] && currentVersion === version && !version) {
    return Promise.resolve();
  }

  try {
    // Use version parameter or timestamp to prevent caching
    const versionParam = version ? `?v=${version}` : `?t=${Date.now()}`;
    const filePath = path.join(process.cwd(), 'html', 'locales', language, `${namespace}.json`);

    // Check if file exists and get modification time
    const fileStats = await fs.promises.stat(filePath).catch(() => null);
    const fileModTime = fileStats ? fileStats.mtime.getTime() : 0;

    // If we have current version and file hasn't changed, skip loading
    if (currentVersion && fileModTime <= parseInt(currentVersion)) {
      if (loadedNamespaces) loadedNamespaces[cacheKey] = true;
      return Promise.resolve();
    }

    // Load and parse the file with BOM handling
    const content = await fs.promises.readFile(filePath, 'utf8');
    const translations = parseJSONWithBOM(content);

    // Update i18next resources
    i18next.addResourceBundle(language, namespace, translations, true, true);

    // Mark namespace as loaded and update version
    if (loadedNamespaces) loadedNamespaces[cacheKey] = true;
    namespaceVersions.set(cacheKey, fileModTime.toString());

    console.log(`[i18n] Loaded namespace: ${language}:${namespace} (version: ${fileModTime})`);
    return Promise.resolve();
  } catch (error) {
    console.error(`Failed to load namespace ${namespace} for ${language}:`, error);
    return Promise.reject(error);
  }
}

function processTranslationQueue() {
  if (translationQueue.isEmpty()) {
    setTimeout(processTranslationQueue, 5000);
    return;
  }

  console.log(`[i18n] Processing translation queue: ${translationQueue.size()} items`);

  // Group items by language and namespace to process in batches
  const batches = {};
  const batchSize = 20; // Process 20 items at a time
  const processedItems = [];

  // Dequeue multiple items at once
  for (let i = 0; i < batchSize && !translationQueue.isEmpty(); i++) {
    const item = translationQueue.dequeue();
    if (!item) continue;

    // Skip items with invalid target language
    const defaultLanguage = process.env.DEFAULT_LANGUAGE || 'en';
    if (!item.targetLang || item.targetLang === defaultLanguage) {
      console.log(`[i18n] Skipping item with invalid target language: ${item.targetLang}`);
      continue;
    }

    const batchKey = `${item.targetLang}:${item.namespace || 'common'}`;

    if (!batches[batchKey]) {
      batches[batchKey] = {
        targetLang: item.targetLang,
        namespace: item.namespace || 'common',
        items: []
      };
    }

    batches[batchKey].items.push(item);
    processedItems.push(item);
  }

  // Process each batch
  const batchPromises = Object.values(batches).map(async (batch) => {
    try {
      console.log(`[i18n] Processing batch for [${batch.targetLang}] ${batch.namespace} with ${batch.items.length} items`);

      // Extract texts to translate
      const texts = batch.items.map(item => item.text);

      // Directly call translateText or batchTranslate without using i18next
      const translatedTexts = await batchTranslate(
        texts,
        batch.targetLang,
        batch.namespace // Use namespace as context
      );

      // Make sure we have the same number of translations as source texts
      if (translatedTexts.length !== batch.items.length) {
        throw new Error(`Translation count mismatch: expected ${batch.items.length}, got ${translatedTexts.length}`);
      }

      // Prepare translations for saving to file
      const fileUpdates = {};

      translatedTexts.forEach((translatedText, index) => {
        const item = batch.items[index];
        const namespaceFile = item.namespace || 'common';

        if (!fileUpdates[namespaceFile]) {
          fileUpdates[namespaceFile] = [];
        }

        fileUpdates[namespaceFile].push({
          key: item.key,
          value: translatedText
        });

        console.log(`[i18n] Translated [${batch.targetLang}] ${item.key}: "${translatedText.substring(0, 30)}..."`);
      });

      // Save translations to files (one file write per namespace)
      for (const [namespace, updates] of Object.entries(fileUpdates)) {
        // Path to translation file
        const filePath = path.join(process.cwd(), 'html', 'locales', batch.targetLang, `${namespace}.json`);

        // Read existing translations
        let translations = {};
        try {
          if (fs.existsSync(filePath)) {
            const content = fs.readFileSync(filePath, 'utf8');
            translations = parseJSONWithBOM(content);
          }
        } catch (error) {
          console.error(`[i18n] Error reading translation file: ${error.message}`);
          // Continue with empty translations if file can't be read
        }

        // Apply all updates to the translation object
        let updateCount = 0;
        updates.forEach(update => {
          const keyPath = update.key.split('.');
          let current = translations;

          // Create nested objects for the key path
          for (let i = 0; i < keyPath.length - 1; i++) {
            const segment = keyPath[i];
            if (!current[segment]) {
              current[segment] = {};
            }
            current = current[segment];
          }

          // Set the translation
          current[keyPath[keyPath.length - 1]] = update.value;
          updateCount++;
        });

        // Ensure directory exists
        const dir = path.dirname(filePath);
        if (!fs.existsSync(dir)) {
          fs.mkdirSync(dir, { recursive: true });
        }

        // Write updated translations to file
        fs.writeFileSync(filePath, JSON.stringify(translations, null, 2), 'utf8');

        console.log(`[i18n] Saved: [${batch.targetLang}] ${namespace} (${updateCount} items)`);
      }

      // Mark all items as processed
      batch.items.forEach(item => {
        translationQueue.markProcessed(item, true);
      });
    } catch (error) {
      console.error(`[i18n] Batch processing error for [${batch.targetLang}]: ${error.message}`);

      // Mark all items as failed
      batch.items.forEach(item => {
        translationQueue.markProcessed(item, false);
      });
    }
  });

  // Wait for all batches to complete
  Promise.allSettled(batchPromises)
    .then(() => {
      // Continue processing queue immediately if there are more items
      if (!translationQueue.isEmpty()) {
        setImmediate(processTranslationQueue);
      } else {
        // Otherwise, schedule next check after a short delay
        setTimeout(processTranslationQueue, 1000);
      }
    });
}

// Initialize global variable for default language
const defaultLanguage = process.env.DEFAULT_LANGUAGE || 'en';

/**
 * Initialize i18next for Express
 * @param {Object} options - Additional settings
 * @returns {Function} Middleware for Express
 */
function initI18n(options = {}) {
  // Initialize loaded namespaces tracking
  global.loadedNamespaces = {};

  // FIXED PATH: Using html/locales instead of just locales
  const localesPath = path.join(process.cwd(), 'html', 'locales');

  // List of all supported languages
  const supportedLngs = [
    'en', 'de', 'tr', 'pl', 'fr', 'it', 'es', 'ru', 'ar', 'zh', 'ro', 'hr', 'bg', 'hi', 'ja', 'ko', 'cs',
    'nl', 'el', 'pt', 'he', 'hu', 'sv', 'da', 'fi', 'sk', 'lt', 'lv', 'et', 'sl', 'uk', 'sr', 'bs', 'no'
  ];

  // Namespaces list
  const namespaces = ['common', 'rma', 'dashboard', 'auth'];

  // Initialize i18next
  i18next
    .use(Backend)
    .use(i18nextMiddleware.LanguageDetector)
    .init({
      backend: {
        loadPath: path.join(localesPath, '{{lng}}', '{{ns}}.json'),
        addPath: path.join(localesPath, '{{lng}}', '{{ns}}.missing.json'),
        parse: (data) => parseJSONWithBOM(data) // Use BOM-aware parser
      },
      fallbackLng: false,
      preload: supportedLngs,
      ns: namespaces,
      defaultNS: 'common',
      detection: {
        // Updated detection order with customHeader first
        order: ['customHeader', 'querystring', 'cookie', 'header'],
        // Set customHeader to look for app-language
        lookupCustomHeader: 'app-language',
        lookupCookie: 'i18next',
        lookupQuerystring: 'lang',
        lookupHeader: 'accept-language',
        lookupSession: 'lang',
        caches: ['cookie'],
        cookieExpirationDate: new Date(Date.now() + 365 * 24 * 60 * 60 * 1000), // 1 year
        cookieDomain: options?.cookieDomain || undefined,
        // Add a callback to log the detected language in dev mode
        lookupFromRequest: (req) => {
          // This will run after language detection
          // 1. Читаем исходный язык в новую константу rawDetectedLanguage. 
          //    ВАЖНО: Добавляем полную цепочку fallback для надежности, 
          //    так как просто req.language может быть undefined в этом месте.
          const rawDetectedLanguage = req.language || req.i18n?.language || process.env.DEFAULT_LANGUAGE || 'en';

          // 2. Нормализуем rawDetectedLanguage и присваиваем результат 
          //    константе с ОРИГИНАЛЬНЫМ именем 'detectedLanguage'.
          const detectedLanguage = (typeof rawDetectedLanguage === 'string' && rawDetectedLanguage.includes('-'))
            ? rawDetectedLanguage.split('-')[0]
            : rawDetectedLanguage;
          if (process.env.NODE_ENV === 'development') {
            let source = 'unknown';
            if (req.headers['app-language']) {
              source = `customHeader (app-language: ${req.headers['app-language']})`;
            } else if (req.query.lang) {
              source = `querystring (lang: ${req.query.lang})`;
            } else if (req.cookies.i18next) {
              source = `cookie (i18next: ${req.cookies.i18next})`;
            } else if (req.headers['accept-language']) {
              source = `header (accept-language: ${req.headers['accept-language']})`;
            }

            console.log(`[i18n] Language detected: ${detectedLanguage} from ${source} (path: ${req.path})`);

          }
          return detectedLanguage;
        }
      },
      load: 'languageOnly',
      saveMissing: true,
      // Return key as is for client-side handling
      parseMissingKeyHandler: (key, defaultValue) => {
        return defaultValue;
      },
      missingKeyHandler: (lng, ns, key, fallbackValue, options, req) => {
        // Get the primary language from the array or use the language if it's already a string
        // 1. Сначала получаем первичный язык из параметра 'lng' 
        //    (обрабатываем случай, если lng - массив)
        const detectedLng = Array.isArray(lng) ? lng[0] : lng;

        // 2. Теперь создаем НУЖНУЮ вам константу 'targetLanguage',
        //    применяя нормализацию к 'detectedLng'
        const targetLanguage = (typeof detectedLng === 'string' && detectedLng.includes('-'))
          ? detectedLng.split('-')[0] // Берем 'de' из 'de-DE'
          : detectedLng;            // Или оставляем 'de', 'en' как есть

        const defaultLanguage = process.env.DEFAULT_LANGUAGE || 'en';

        // Make sure we have a valid target language that's not the default
        if (!targetLanguage || targetLanguage === defaultLanguage) {
          return;  // Skip processing for default language
        }

        console.log(`Missing translation detected: [${targetLanguage}] ${ns}:${key}`);

        // Create a unique key to track this missing key
        const uniqueKey = `${targetLanguage}:${ns}:${key}`;

        try {
          // Mark that we're processing this key
          processingKeys.add(uniqueKey);

          // Add to queue only needed data, not relying on req
          translationQueue.enqueue({
            text: fallbackValue,
            targetLang: targetLanguage,
            namespace: ns,
            key: key
          });

          // FIX: Remove reference to undefined sourceType variable
          console.log(`[i18n] Added to translation queue: [${targetLanguage}] ${ns}:${key}`);
        } finally {
          // Always remove from processing list when done
          processingKeys.delete(uniqueKey);
        }
      },
      interpolation: {
        escapeValue: false,
        formatSeparator: ',',
        format: function (value, format, lng) {
          if (format === 'uppercase') return value.toUpperCase();
          return value;
        }
      },
      ...options
    });

  // Promise-based translation function
  i18next.getTranslationAsync = async (key, options, lng) => {
    return new Promise((resolve) => {
      i18next.t(key, { ...options, lng }, (err, translation) => {
        resolve(translation);
      });
    });
  };

  // Start queue processor
  processTranslationQueue();

  // NEW: We ONLY return the i18next middleware here, not the tag processor
  return i18nextMiddleware.handle(i18next);
}

module.exports = initI18n;
module.exports.i18next = i18next;
module.exports.translationQueue = translationQueue;