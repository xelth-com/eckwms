// middleware/i18n.js [UPDATED VERSION]
const i18next = require('i18next');
const i18nextMiddleware = require('i18next-http-middleware');
const Backend = require('i18next-fs-backend');
const path = require('path');
const { translateText, saveToCache, batchTranslate } = require('../services/translationService');

const { Queue } = require('../utils/queue');
const fs = require('fs');
const { stripBOM, parseJSONWithBOM } = require('../utils/bomUtils');

// Create translation queue for deferred translations
const translationQueue = new Queue();
const namespaceVersions = new Map();
// Track keys currently being processed to prevent recursion
const processingKeys = new Set();

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

// Improved processTranslationQueue function with better logging and error handling
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

    // Ensure we have a valid target language that's not the default
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
      
      // Extract texts and save item mapping
      const texts = batch.items.map(item => item.text);
      
      // Use batchTranslate function to translate all texts at once
      const translatedTexts = await batchTranslate(
        texts,
        batch.targetLang,
        batch.namespace, // Use namespace as context
        'en' // Source language is English
      );

      // Prepare translations for saving to file
      const fileUpdates = {};
      
      // Make sure we have the same number of translations as source texts
      if (translatedTexts.length === batch.items.length) {
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
      } else {
        throw new Error(`Translation count mismatch: expected ${batch.items.length}, got ${translatedTexts.length}`);
      }

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
  const defaultLanguage = process.env.DEFAULT_LANGUAGE || 'en';

  // List of all supported languages
  const supportedLngs = [
    // EU official languages
    'de', 'en', 'fr', 'it', 'es', 'pt', 'nl', 'da', 'sv', 'fi',
    'el', 'cs', 'pl', 'hu', 'sk', 'sl', 'et', 'lv', 'lt', 'ro',
    'bg', 'hr', 'ga', 'mt',
    // Additional languages
    'ru', 'tr', 'ar', 'zh', 'uk', 'sr', 'he', 'ko', 'ja'
  ];

  // Optimized language detection middleware
  const languageDetectorMiddleware = (req, res, next) => {
    // First, quickly check if this is likely an HTML request
    const acceptHeader = req.headers['accept'] || '';
    const path = req.path || '';

    // Skip non-HTML requests early
    const isLikelyHtml =
      acceptHeader.includes('text/html') ||
      path.endsWith('.html') ||
      path === '/' ||
      (!path.includes('.') && !path.startsWith('/api/'));

    if (!isLikelyHtml) {
      // Even for non-HTML requests, still set the default language
      req.language = defaultLanguage;
      next();
      return;
    }

    // For HTML requests, perform language detection
    const userLanguage =
      req.cookies?.i18next ||
      req.query?.lang ||
      req.headers['accept-language']?.split(',')[0]?.split('-')[0] ||
      defaultLanguage;

    // Check if language is supported
    req.language = supportedLngs.includes(userLanguage) ? userLanguage : defaultLanguage;

    // Ensure req.language is always set (debug)
    console.log(`[i18n] Language detected: ${req.language}`);

    // Save language in cookie if it changed
    if (req.cookies?.i18next !== req.language) {
      res.cookie('i18next', req.language, {
        maxAge: 365 * 24 * 60 * 60 * 1000, // 1 year
        path: '/'
      });
    }

    next();
  };

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
        order: ['cookie', 'querystring', 'header', 'session'],
        lookupCookie: 'i18next',
        lookupQuerystring: 'lang',
        lookupHeader: 'accept-language',
        lookupSession: 'lang',
        caches: ['cookie'],
        cookieExpirationDate: new Date(Date.now() + 365 * 24 * 60 * 60 * 1000), // 1 year
        cookieDomain: options.cookieDomain || undefined
      },
      load: 'languageOnly',
      saveMissing: true,
      // Return key as is for client-side handling
      parseMissingKeyHandler: (key, defaultValue) => {
        return key;
      },
      // Find this function in middleware/i18n.js and replace it with this version
      missingKeyHandler: (lng, ns, key, fallbackValue, options, req) => {
        // Get the primary language from the array or use the language if it's already a string
        const targetLanguage = Array.isArray(lng) ? lng[0] : lng;

        // Make sure we have a valid target language that's not the default
        if (!targetLanguage || targetLanguage === defaultLanguage) {
          return;  // Skip processing for default language
        }

        console.log(`Missing translation detected: [${targetLanguage}] ${ns}:${key}`);

        // Create a unique key to track this missing key
        const uniqueKey = `${targetLanguage}:${ns}:${key}`;

        // Check if the translation already exists in the files
        try {
          // Check if the key exists in the target language
          if (i18next.exists(key, { ns, lng: targetLanguage })) {
            if (process.env.NODE_ENV === 'development') {
              console.log(`[i18n] Translation already exists for ${targetLanguage}:${ns}:${key}, skipping queue`);
            }
            return; // Translation exists, skip queueing
          }
        } catch (err) {
          console.error(`[i18n] Error checking if translation exists: ${err.message}`);
        }

        // Prevent recursive calls - skip if we're already processing this key
        if (processingKeys.has(uniqueKey)) {
          return;
        }

        try {
          // Mark that we're processing this key
          processingKeys.add(uniqueKey);

          // Get text from default language WITHOUT triggering missing key handler
          let defaultText = key;
          let foundInDefaultLang = false;
          let sourceType = 'key'; // Track where we found the source text

          // 1. Check if the key exists in the default language
          if (i18next.exists(key, { ns, lng: defaultLanguage })) {
            defaultText = i18next.t(key, { ns, lng: defaultLanguage });
            foundInDefaultLang = true;
            sourceType = 'defaultLang';
          }
          // 2. Check if the content was saved in elementContents map
          else if (global.elementContents && global.elementContents.has(key)) {
            defaultText = global.elementContents.get(key);
            sourceType = 'elementMap';
            console.log(`Found content in elementContents map for key ${key}: "${defaultText}"`);
          }
          // 3. Try to extract from HTML as a last resort
          else if (global.currentProcessingHtml) {
            try {
              // Look for HTML element with this data-i18n key
              const regex = new RegExp(`<[^>]+data-i18n=["']${key}["'][^>]*>([^<]+)<\/[^>]+>`, 'g');
              const match = regex.exec(global.currentProcessingHtml);

              if (match && match[1]) {
                defaultText = match[1].trim();
                sourceType = 'htmlContent';
                console.log(`Found content in HTML for key ${key}: "${defaultText}"`);
              }
            } catch (error) {
              console.error(`Error extracting HTML content for ${key}:`, error);
            }
          }

          // Important fix: Use the correct target language for the queue
          console.log(`Adding to translation queue: [${targetLanguage}] ${ns}:${key} (source: ${sourceType})`);
          translationQueue.enqueue({
            text: defaultText,
            targetLang: targetLanguage, // Use the request's target language, not defaultLanguage
            namespace: ns,
            key: key,
            sourceType: sourceType
          });

          if (process.env.NODE_ENV === 'development') {
            console.log(`[i18n] Added to translation queue: [${targetLanguage}] ${ns}:${key} (source: ${sourceType})`);
          }
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

  // Middleware for HTML response processing and i18n tag replacement
  const tagProcessor = (req, res, next) => {
    console.log('tagProcessor middleware called');

    // Store original methods
    const originalSend = res.send;
    const originalRender = res.render;
    const originalJson = res.json;
    const originalEnd = res.end;

    // Helper function to process HTML content
    const processHtmlContent = (body) => {
      // Get language from request (set by i18next-http-middleware)
      const language = req.language || defaultLanguage;

      if (language === defaultLanguage || typeof body !== 'string') {
        return body;
      }

      console.log(`Processing HTML content for language: ${language}`);

      // Count translation tags to see if any exist
      const i18nTagCount = (body.match(/data-i18n=/g) || []).length;
      const i18nAttrCount = (body.match(/data-i18n-attr=/g) || []).length;
      const i18nHtmlCount = (body.match(/data-i18n-html=/g) || []).length;

      console.log(`Found translation tags: ${i18nTagCount} standard, ${i18nAttrCount} attribute, ${i18nHtmlCount} HTML`);

      if (i18nTagCount + i18nAttrCount + i18nHtmlCount === 0) {
        console.log('No translation tags found in HTML');
        return body;
      }

      // Process tags with multiple pattern support for different element types
      // 1. Simple elements with text content
      body = body.replace(/<([^>]+)\s+data-i18n="([^"]+)"([^>]*)>([^<]*)<\/([^>]+)>/g, (match, tag1, key, attrs, content, tag2) => {
        // Try to get translation - namespace may be included in the key
        let namespace = 'common';
        let translationKey = key;

        if (key.includes(':')) {
          const parts = key.split(':');
          namespace = parts[0];
          translationKey = parts.slice(1).join(':');
        }

        console.log(`Translating: ${translationKey} in namespace ${namespace}`);

        try {
          // Add safeguard against infinite recursion
          const uniqueKey = `${language}:${namespace}:${translationKey}`;
          if (processingKeys.has(uniqueKey)) {
            return match; // Skip if already processing this key
          }

          processingKeys.add(uniqueKey);
          const translation = req.i18n.t(translationKey, { ns: namespace });
          processingKeys.delete(uniqueKey);

          // If translation equals key (not found), leave as is for frontend to handle
          if (translation === translationKey) {
            return match; // Keep original tag for frontend retry
          }

          console.log(`Translated: ${translationKey} → ${translation}`);
          // Return element with translation but REMOVE the data-i18n attribute
          return `<${tag1}${attrs}>${translation}</${tag2}>`;
        } catch (error) {
          console.error(`Error translating ${translationKey}:`, error);
          return match; // Return original on error
        }
      });

      // 2. Process data-i18n-attr attributes
      body = body.replace(/<([^>]+)\s+data-i18n-attr=['"]([^'"]+)['"]([^>]*)>/g, (match, tag, attrsJson, restAttrs) => {
        try {
          // Parse with BOM handling
          const attrsMap = parseJSONWithBOM(attrsJson);
          let newTag = `<${tag}${restAttrs}`;
          let allTranslated = true;

          for (const [attr, key] of Object.entries(attrsMap)) {
            console.log(`Translating attribute: ${attr} with key ${key}`);
            // Extract namespace if present
            let namespace = 'common';
            let translationKey = key;

            if (key.includes(':')) {
              const parts = key.split(':');
              namespace = parts[0];
              translationKey = parts.slice(1).join(':');
            }

            // Add safeguard against infinite recursion
            const uniqueKey = `${language}:${namespace}:${translationKey}`;
            if (processingKeys.has(uniqueKey)) {
              allTranslated = false;
              continue; // Skip if already processing this key
            }

            try {
              processingKeys.add(uniqueKey);
              const translation = req.i18n.t(translationKey, { ns: namespace });
              processingKeys.delete(uniqueKey);

              // Get current attribute value if present
              const attrRegex = new RegExp(`${attr}="([^"]*)"`, 'i');
              const attrValueMatch = match.match(attrRegex);
              const currentValue = attrValueMatch ? attrValueMatch[1] : '';

              // If translation differs from key, replace attribute value
              if (translation !== translationKey) {
                console.log(`Translated attr: ${translationKey} → ${translation}`);
                if (attrValueMatch) {
                  newTag = newTag.replace(
                    `${attr}="${currentValue}"`,
                    `${attr}="${translation}"`
                  );
                } else {
                  // Attribute not present, add it
                  newTag = newTag + ` ${attr}="${translation}"`;
                }
              } else {
                allTranslated = false; // Mark that not all attributes are translated
              }
            } catch (error) {
              console.error(`Error translating attribute ${translationKey}:`, error);
              allTranslated = false;
            }
          }

          // Keep data-i18n-attr for frontend retries if not all translated
          if (!allTranslated) {
            return newTag + '>';
          }

          // Otherwise remove data-i18n-attr but keep the tag
          return newTag.replace(/\s+data-i18n-attr=['"][^'"]+['"]/, '') + '>';
        } catch (e) {
          console.error('Error parsing data-i18n-attr:', e);
          return match;
        }
      });

      // 3. Process elements with HTML content
      body = body.replace(/<([^>]+)\s+data-i18n-html="([^"]+)"([^>]*)>([\s\S]*?)<\/([^>]+)>/g, (match, tag1, key, attrs, content, tag2) => {
        // Apply the same namespace extraction logic for HTML content
        let namespace = 'common';
        let translationKey = key;

        if (key.includes(':')) {
          const parts = key.split(':');
          namespace = parts[0];
          translationKey = parts.slice(1).join(':');
        }

        console.log(`Translating HTML: ${translationKey} in namespace ${namespace}`);

        try {
          // Add safeguard against infinite recursion
          const uniqueKey = `${language}:${namespace}:${translationKey}`;
          if (processingKeys.has(uniqueKey)) {
            return match; // Skip if already processing this key
          }

          processingKeys.add(uniqueKey);
          const translation = req.i18n.t(translationKey, {
            ns: namespace,
            interpolation: { escapeValue: false }
          });
          processingKeys.delete(uniqueKey);

          // If translation equals key (not found), leave as is for frontend to handle
          if (translation === translationKey) {
            return match;
          }

          console.log(`Translated HTML: ${translationKey}`);
          // Return element with HTML translation and REMOVE the data-i18n-html attribute
          return `<${tag1}${attrs}>${translation}</${tag2}>`;
        } catch (error) {
          console.error(`Error translating HTML ${translationKey}:`, error);
          return match; // Return original on error
        }
      });

      return body;
    };

    // Override res.send
    res.send = function (body) {
      // Check if response is likely HTML
      if (typeof body === 'string' &&
        (res.get('Content-Type')?.includes('text/html') ||
          body.includes('<!DOCTYPE html>') ||
          body.includes('<html>'))) {

        // Process HTML content
        body = processHtmlContent(body);
      }

      // Call original method
      return originalSend.call(this, body);
    };

    // Override res.render to handle template rendering
    res.render = function (view, options, callback) {
      console.log('res.render called');

      // If callback is provided, intercept the rendered HTML
      if (typeof callback === 'function') {
        const originalCallback = callback;
        callback = function (err, html) {
          if (!err && html) {
            html = processHtmlContent(html);
          }
          originalCallback(err, html);
        };
      } else if (typeof options === 'function') {
        // Handle case where options is the callback
        const originalCallback = options;
        options = {};
        callback = function (err, html) {
          if (!err && html) {
            html = processHtmlContent(html);
          }
          originalCallback(err, html);
        };
      } else {
        // No callback, use events to intercept response
        const self = this;
        const originalEnd = res.end;

        res.end = function (chunk, encoding) {
          if (chunk && typeof chunk === 'string') {
            chunk = processHtmlContent(chunk);
          }
          return originalEnd.call(self, chunk, encoding);
        };
      }

      // Call original render
      return originalRender.call(this, view, options, callback);
    };

    // We should also consider intercepting res.end for direct responses
    res.end = function (chunk, encoding) {
      console.log('res.end called');

      if (chunk && typeof chunk === 'string' &&
        (res.get('Content-Type')?.includes('text/html') ||
          chunk.includes('<!DOCTYPE html>') ||
          chunk.includes('<html>'))) {

        chunk = processHtmlContent(chunk);
      }

      return originalEnd.call(this, chunk, encoding);
    };

    next();
  };

  // Combine i18next middleware with our tag processor
  return [languageDetectorMiddleware, i18nextMiddleware.handle(i18next), tagProcessor];
}

module.exports = initI18n;
module.exports.i18next = i18next;
module.exports.translationQueue = translationQueue;