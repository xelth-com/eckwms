// middleware/i18n.js
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
    // EU official languages
    'de', 'en', 'fr', 'it', 'es', 'pt', 'nl', 'da', 'sv', 'fi',
    'el', 'cs', 'pl', 'hu', 'sk', 'sl', 'et', 'lv', 'lt', 'ro',
    'bg', 'hr', 'ga', 'mt',
    // Additional languages
    'ru', 'tr', 'ar', 'zh', 'uk', 'sr', 'he', 'ko', 'ja'
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
        cookieDomain: options.cookieDomain || undefined,
        // Add a callback to log the detected language in dev mode
        lookupFromRequest: (req) => {
          // This will run after language detection
          const detectedLanguage = req.language;
          if (process.env.NODE_ENV === 'development') {
            console.log(`[i18n] Language detected: ${detectedLanguage} (request path: ${req.path})`);
          }
          return detectedLanguage;
        }
      },
      load: 'languageOnly',
      saveMissing: true,
      // Return key as is for client-side handling
      parseMissingKeyHandler: (key, defaultValue) => {
        return key;
      },
      // Внутри инициализации i18next в middleware/i18n.js
      // В конфигурации i18next
      missingKeyHandler: (lng, ns, key, fallbackValue, options, req) => {
        // Get the primary language from the array or use the language if it's already a string
        const targetLanguage = Array.isArray(lng) ? lng[0] : lng;
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

          // Get text from default language or other sources
          let defaultText = key;
          let sourceType = 'key';

          // 1. Try to get text from default language via req.i18n
          if (req && req.i18n) {
            try {
              if (req.i18n.exists(key, { ns, lng: defaultLanguage })) {
                defaultText = req.i18n.t(key, { ns, lng: defaultLanguage });
                sourceType = 'defaultLang';
              }
            } catch (error) {
              console.error(`Error getting default text from i18n: ${error.message}`);
            }
          }

          // 2. Check if the content was saved in elementContents map
          if (sourceType === 'key' && req && req.elementContents && req.elementContents.has(key)) {
            defaultText = req.elementContents.get(key);
            sourceType = 'elementMap';
          }

          // 3. Try to extract from HTML as a last resort
          if (sourceType === 'key' && req && req.currentProcessingHtml) {
            try {
              const regex = new RegExp(`<[^>]+data-i18n=["']${key}["'][^>]*>([^<]+)<\/[^>]+>`, 'g');
              const match = regex.exec(req.currentProcessingHtml);
              if (match && match[1]) {
                defaultText = match[1].trim();
                sourceType = 'htmlContent';
              }
            } catch (error) {
              console.error(`Error extracting HTML content: ${error.message}`);
            }
          }

          // Добавляем в очередь только необходимые данные, не полагаясь на req
          translationQueue.enqueue({
            text: defaultText,
            targetLang: targetLanguage,
            namespace: ns,
            key: key,
            sourceType: sourceType
            // Не сохраняем req, так как он нам больше не нужен
          });

          console.log(`[i18n] Added to translation queue: [${targetLanguage}] ${ns}:${key} (source: ${sourceType})`);
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

  // Enhanced tagProcessor middleware for HTML response processing 
  // Combines functionality from htmlInterceptor.js
  const tagProcessor = (req, res, next) => {
    console.log('[i18n] tagProcessor middleware called for path:', req.path);
    console.log(`[i18n] Current language: ${req.language || defaultLanguage}`);

    // Store original methods
    const originalSend = res.send;
    const originalRender = res.render;
    const originalJson = res.json;
    const originalEnd = res.end;
    
    // Initialize request-specific data storage
    req.elementContents = new Map();

    // Define a new function to detect HTML content
    const isHtmlContent = (content) => {
      if (!content || typeof content !== 'string') return false;
      
      // Content-Type based detection
      const contentType = res.get('Content-Type');
      const contentTypeIsHtml = contentType?.includes('text/html') || contentType?.includes('text/plain');
      
      // Structure-based detection
      const hasHtmlStructure = 
        content.includes('<!DOCTYPE html>') ||
        content.includes('<html') ||
        content.includes('<head') ||
        content.includes('<body');
        
      // Translation tag detection (this is the most important for our use case)
      const hasTranslationTags = 
        content.includes('data-i18n=') ||
        content.includes('data-i18n-attr=') ||
        content.includes('data-i18n-html=');
        
      // Special case for fragments that have translation tags but aren't full HTML documents
      const isTranslatableFragment = hasTranslationTags;
      
      // Special case for known content
      const containsKnownAppContent = 
        content.includes('M3mobile') || 
        content.includes('RMA') || 
        content.includes('class="text3"');
        
      const result = contentTypeIsHtml || hasHtmlStructure || isTranslatableFragment || containsKnownAppContent;
      
      console.log(`[i18n] HTML detection: ContentType=${contentTypeIsHtml}, Structure=${hasHtmlStructure}, Tags=${hasTranslationTags}, Fragment=${isTranslatableFragment}, AppContent=${containsKnownAppContent} => Result=${result}`);
      
      return result;
    };

    // Helper function to process HTML content with enhanced debugging
    const processHtmlContent = (body) => {
      // Store the current HTML for potential extraction of missing translations
      req.currentProcessingHtml = body;

      // Get language from request (set by i18next-http-middleware)
      const language = req.language || defaultLanguage;
      console.log(`[i18n] Processing HTML content for language: ${language}`);

      if (language === defaultLanguage) {
        console.log(`[i18n] Using default language (${defaultLanguage}), skipping translation processing`);
        return body;
      }
      
      if (typeof body !== 'string') {
        console.log(`[i18n] Body is not a string (type: ${typeof body}), skipping translation processing`);
        return body;
      }

      // Build application configuration - moved from htmlInterceptor.js
      let modifiedBody = body;
      if (modifiedBody.includes('<head>')) {
        // Build application configuration object
        const appConfig = {
          DEFAULT_LANGUAGE: process.env.DEFAULT_LANGUAGE || 'en',
          NODE_ENV: process.env.NODE_ENV || 'development',
          // Add any other configuration you need
          API_BASE_URL: process.env.API_BASE_URL || '',
          APP_VERSION: process.env.npm_package_version || '1.0.0'
        };
        
        const configScript = `<head>
<script>
// Global app configuration
window.APP_CONFIG = ${JSON.stringify(appConfig)};
console.log("App config loaded:", window.APP_CONFIG);
</script>`;
        
        modifiedBody = modifiedBody.replace('<head>', configScript);
      }

      console.log(`Processing HTML content for language: ${language}`);

      // Count translation tags to see if any exist
      const i18nTagCount = (modifiedBody.match(/data-i18n=/g) || []).length;
      const i18nAttrCount = (modifiedBody.match(/data-i18n-attr=/g) || []).length;
      const i18nHtmlCount = (modifiedBody.match(/data-i18n-html=/g) || []).length;

      console.log(`[i18n] Found translation tags: ${i18nTagCount} standard, ${i18nAttrCount} attribute, ${i18nHtmlCount} HTML`);
      
      // Log a sample of the content to help with debugging
      if (process.env.NODE_ENV === 'development') {
        const bodyPreview = modifiedBody.substring(0, 200).replace(/\n/g, '\\n') + '...';
        console.log(`[i18n] Content preview: ${bodyPreview}`);
      }

      if (i18nTagCount + i18nAttrCount + i18nHtmlCount === 0) {
        console.log('[i18n] No translation tags found in HTML, returning unmodified content');
        return modifiedBody;
      }

      // First step: Extract all content from elements with data-i18n attributes
      // This pre-processing step makes content available for missingKeyHandler
      modifiedBody.replace(/<([^>]+)\s+data-i18n="([^"]+)"([^>]*)>([^<]*)<\/([^>]+)>/g, 
        (match, tag1, key, attrs, content, tag2) => {
          if (content && content.trim()) {
            // Store the content by key for the missingKeyHandler to use
            req.elementContents.set(key, content.trim());
            if (process.env.NODE_ENV === 'development') {
              console.log(`Stored content for key ${key}: "${content.trim()}"`);
            }
          }
          return match; // Return unchanged, this is just for extraction
        }
      );

      // Process tags with multiple pattern support for different element types
      // 1. Simple elements with text content
      modifiedBody = modifiedBody.replace(/<([^>]+)\s+data-i18n="([^"]+)"([^>]*)>([^<]*)<\/([^>]+)>/g, (match, tag1, key, attrs, content, tag2) => {
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
          
          const processingKeys = req.processingKeys || new Set();
          
          if (processingKeys.has(uniqueKey)) {
            return match; // Skip if already processing this key
          }
          
          processingKeys.add(uniqueKey);
          req.processingKeys = processingKeys;
          
          const translation = req.i18n.t(translationKey, { ns: namespace });
          
          processingKeys.delete(uniqueKey);
          req.processingKeys = processingKeys;

          // If translation equals key (not found), leave as is for frontend to handle
          if (translation === translationKey) {
            return match; // Keep original tag for frontend retry
          }

          console.log(`Translated: ${translationKey} → ${translation}`);
          // Return element with translation but KEEP the data-i18n attribute for client-side processing
          return `<${tag1} data-i18n="${key}"${attrs}>${translation}</${tag2}>`;
        } catch (error) {
          console.error(`Error translating ${translationKey}:`, error);
          return match; // Return original on error
        }
      });

      // 2. Process data-i18n-attr attributes
      modifiedBody = modifiedBody.replace(/<([^>]+)\s+data-i18n-attr=['"]([^'"]+)['"]([^>]*)>/g, (match, tag, attrsJson, restAttrs) => {
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
            const processingKeys = req.processingKeys || new Set();
            
            if (processingKeys.has(uniqueKey)) {
              allTranslated = false;
              continue; // Skip if already processing this key
            }
            
            try {
              processingKeys.add(uniqueKey);
              req.processingKeys = processingKeys;
              
              const translation = req.i18n.t(translationKey, { ns: namespace });
              
              processingKeys.delete(uniqueKey);
              req.processingKeys = processingKeys;

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

          // Always keep data-i18n-attr for frontend processing
          return newTag + '>';
        } catch (e) {
          console.error('Error parsing data-i18n-attr:', e);
          return match;
        }
      });

      // 3. Process elements with HTML content
      modifiedBody = modifiedBody.replace(/<([^>]+)\s+data-i18n-html="([^"]+)"([^>]*)>([\s\S]*?)<\/([^>]+)>/g, (match, tag1, key, attrs, content, tag2) => {
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
          const processingKeys = req.processingKeys || new Set();
          
          if (processingKeys.has(uniqueKey)) {
            return match; // Skip if already processing this key
          }
          
          processingKeys.add(uniqueKey);
          req.processingKeys = processingKeys;
          
          const translation = req.i18n.t(translationKey, {
            ns: namespace,
            interpolation: { escapeValue: false }
          });
          
          processingKeys.delete(uniqueKey);
          req.processingKeys = processingKeys;

          // Store the HTML content for potential translation
          if (content && content.trim()) {
            req.elementContents.set(key, content.trim());
          }

          // If translation equals key (not found), leave as is for frontend to handle
          if (translation === translationKey) {
            return match;
          }

          console.log(`Translated HTML: ${translationKey}`);
          // Return element with HTML translation and KEEP the data-i18n-html attribute
          return `<${tag1} data-i18n-html="${key}"${attrs}>${translation}</${tag2}>`;
        } catch (error) {
          console.error(`Error translating HTML ${translationKey}:`, error);
          return match; // Return original on error
        }
      });

      return modifiedBody;
    };

    // Override res.send with improved content type detection and logging
    res.send = function (body) {
      console.log(`[i18n] res.send called with content type: ${res.get('Content-Type')}`);
      
      // Use the shared isHtmlContent function for detection
      if (typeof body === 'string' && isHtmlContent(body)) {
        console.log(`[i18n] Processing HTML content in res.send, body length: ${body?.length || 0}`);
        
        // Process HTML content
        body = processHtmlContent(body);
      } else if (typeof body === 'string') {
        console.log(`[i18n] Skipping non-HTML string content (first 50 chars): ${body.substring(0, 50)}...`);
      } else {
        console.log(`[i18n] Skipping non-string content of type: ${typeof body}`);
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

    // Override res.end with the same HTML processing logic
    res.end = function (chunk, encoding) {
      console.log('[i18n] res.end called');
      
      if (chunk && typeof chunk === 'string') {
        // Use the shared isHtmlContent function
        if (isHtmlContent(chunk)) {
          console.log(`[i18n] Processing HTML content in res.end, length: ${chunk.length}`);
          chunk = processHtmlContent(chunk);
        }
      }
      
      return originalEnd.call(this, chunk, encoding);
    };

    next();
  };

  // Now only return the i18next middleware and enhanced tag processor
  return [i18nextMiddleware.handle(i18next), tagProcessor];
}

module.exports = initI18n;
module.exports.i18next = i18next;
module.exports.translationQueue = translationQueue;