// middleware/i18n.js
const i18next = require('i18next');
const i18nextMiddleware = require('i18next-http-middleware');
const Backend = require('i18next-fs-backend');
const path = require('path');
const { translateText, saveToCache } = require('../services/translationService');
const { Queue } = require('../utils/queue');
const fs = require('fs');

// Create translation queue for deferred translations
const translationQueue = new Queue();
const namespaceVersions = new Map();

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
  if (loadedNamespaces[cacheKey] && currentVersion === version && !version) {
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
      loadedNamespaces[cacheKey] = true;
      return Promise.resolve();
    }
    
    // Load and parse the file
    const content = await fs.promises.readFile(filePath, 'utf8');
    const translations = JSON.parse(content);
    
    // Update i18next resources
    i18next.addResourceBundle(language, namespace, translations, true, true);
    
    // Mark namespace as loaded and update version
    loadedNamespaces[cacheKey] = true;
    namespaceVersions.set(cacheKey, fileModTime.toString());
    
    console.log(`[i18n] Loaded namespace: ${language}:${namespace} (version: ${fileModTime})`);
    return Promise.resolve();
  } catch (error) {
    console.error(`Failed to load namespace ${namespace} for ${language}:`, error);
    return Promise.reject(error);
  }
}

// Process translation queue
// Further down around line 48-49, update the processTranslationQueue function
function processTranslationQueue() {
  if (translationQueue.isEmpty()) {
    setTimeout(processTranslationQueue, 5000);
    return;
  }

  const item = translationQueue.dequeue();
  if (!item) {
    setTimeout(processTranslationQueue, 100);
    return;
  }

  // No need to track in-progress separately as it's now handled by the Queue
  translateText(item.text, item.targetLang, item.namespace)
    .then(translatedText => {
      // Save translation to localization file
      try {
        // FIXED PATH: Using html/locales instead of just locales
        const filePath = path.join(process.cwd(), 'html', 'locales', item.targetLang, `${item.namespace}.json`);
        // Rest of the saving logic remains the same...

        console.log(`[i18n] Translated and saved: [${item.targetLang}] ${item.namespace}:${item.key}`);
        // Mark as successfully processed
        translationQueue.markProcessed(item, true);
      } catch (error) {
        console.error(`[i18n] Error saving translation: ${error.message}`);
        translationQueue.markProcessed(item, false);
      }
    })
    .catch(error => {
      console.error(`[i18n] Translation error: ${error.message}`);
      translationQueue.markProcessed(item, false);
    })
    .finally(() => {
      // Continue processing queue
      setTimeout(processTranslationQueue, 1000);
    });
}

/**
 * Initialize i18next for Express
 * @param {Object} options - Additional settings
 * @returns {Function} Middleware for Express
 */
function initI18n(options = {}) {
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
  // Создание промежуточного ПО для определения языка
  const languageDetectorMiddleware = (req, res, next) => {
    // Определение языка пользователя с приоритетом:
    // 1. Куки i18next
    // 2. Параметр lang в URL
    // 3. Заголовок Accept-Language
    // 4. Язык по умолчанию
    const userLanguage =
      req.cookies?.i18next ||
      req.query?.lang ||
      req.headers['accept-language']?.split(',')[0]?.split('-')[0] ||
      defaultLanguage;

    // Проверяем, поддерживается ли язык
    req.language = supportedLngs.includes(userLanguage) ? userLanguage : defaultLanguage;

    // Сохраняем язык в куки, если он изменился
    if (req.cookies?.i18next !== req.language) {
      res.cookie('i18next', req.language, {
        maxAge: 365 * 24 * 60 * 60 * 1000, // 1 год
        path: '/'
      });
    }

    console.log(`Language detection: ${req.language} (from: ${req.cookies?.i18next ? 'cookie' : req.query?.lang ? 'query' : 'header'})`);

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
        addPath: path.join(localesPath, '{{lng}}', '{{ns}}.missing.json')
      },
      fallbackLng: defaultLanguage,
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
      missingKeyHandler: (lng, ns, key, fallbackValue) => {
        // Fix: Only queue translations TO non-default languages FROM the default language
        if (lng !== defaultLanguage) {
          // Get the text from the default language
          const defaultText = i18next.t(key, { ns, lng: defaultLanguage }) || key;

          // Use the enhanced Queue's enqueue method
          translationQueue.enqueue({
            text: defaultText,
            targetLang: lng,
            namespace: ns,
            key: key
          });

          if (process.env.NODE_ENV === 'development') {
            console.log(`[i18n] Added to translation queue: [${lng}] ${ns}:${key}`);
          }
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
        const translation = req.i18n.t(translationKey, { ns: namespace });

        // If translation equals key (not found), leave as is for frontend to handle
        if (translation === translationKey) {
          return match; // Keep original tag for frontend retry
        }

        console.log(`Translated: ${translationKey} → ${translation}`);
        // Return element with translation but REMOVE the data-i18n attribute
        return `<${tag1}${attrs}>${translation}</${tag2}>`;
      });

      // 2. Process data-i18n-attr attributes
      body = body.replace(/<([^>]+)\s+data-i18n-attr=['"]([^'"]+)['"]([^>]*)>/g, (match, tag, attrsJson, restAttrs) => {
        try {
          const attrsMap = JSON.parse(attrsJson);
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

            const translation = req.i18n.t(translationKey, { ns: namespace });

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
        const translation = req.i18n.t(translationKey, {
          ns: namespace,
          interpolation: { escapeValue: false }
        });

        // If translation equals key (not found), leave as is for frontend to handle
        if (translation === translationKey) {
          return match;
        }

        console.log(`Translated HTML: ${translationKey}`);
        // Return element with HTML translation and REMOVE the data-i18n-html attribute
        return `<${tag1}${attrs}>${translation}</${tag2}>`;
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