// middleware/i18n.js
const i18next = require('i18next');
const i18nextMiddleware = require('i18next-http-middleware');
const Backend = require('i18next-fs-backend');
const path = require('path');
const { translateText, saveToCache } = require('../services/translationService');
const { Queue } = require('../utils/queue');

// Create translation queue for deferred translations
const translationQueue = new Queue();
const translationInProgress = new Set(); // Track keys being translated

// Process translation queue
function processTranslationQueue() {
  if (translationQueue.isEmpty()) {
    setTimeout(processTranslationQueue, 5000);
    return;
  }

  const { text, targetLang, namespace, key } = translationQueue.dequeue();
  const queueKey = `${targetLang}:${namespace}:${key}`;
  
  // Mark this key as in-progress
  translationInProgress.add(queueKey);
  
  // Translate the text
  translateText(text, targetLang, namespace)
    .then(translatedText => {
      // Save translation to localization file
      try {
        const filePath = path.join(process.cwd(), 'locales', targetLang, `${namespace}.json`);
        let translations = {};
        
        if (require('fs').existsSync(filePath)) {
          translations = require(filePath);
        }
        
        translations[key] = translatedText;
        require('fs').writeFileSync(filePath, JSON.stringify(translations, null, 2), 'utf8');
        
        // Update i18next cache
        i18next.addResourceBundle(targetLang, namespace, { [key]: translatedText }, true, true);
        
        console.log(`[i18n] Translated and saved: [${targetLang}] ${namespace}:${key}`);
      } catch (error) {
        console.error(`[i18n] Error saving translation: ${error.message}`);
      } finally {
        // Remove from in-progress set
        translationInProgress.delete(queueKey);
      }
    })
    .catch(error => {
      console.error(`[i18n] Translation error: ${error.message}`);
      translationInProgress.delete(queueKey);
    })
    .finally(() => {
      // Continue processing queue with slight delay
      setTimeout(processTranslationQueue, 1000);
    });
}

/**
 * Initialize i18next for Express
 * @param {Object} options - Additional settings
 * @returns {Function} Middleware for Express
 */
function initI18n(options = {}) {
  const localesPath = path.join(process.cwd(), 'locales');
  const defaultLanguage = process.env.DEFAULT_LANGUAGE || 'de';
  
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
        loadPath: path.join(localesPath, '{{lng}}', '{{ns}}.json')
      },
      fallbackLng: defaultLanguage,
      preload: supportedLngs,
      ns: namespaces,
      defaultNS: 'common',
      detection: {
        order: ['cookie', 'header', 'querystring', 'session'],
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
        // Only queue untranslated keys from non-default languages
        if (lng !== defaultLanguage) {
          const queueKey = `${lng}:${ns}:${key}`;
          
          // Only add to queue if not already in progress
          if (!translationInProgress.has(queueKey)) {
            translationQueue.enqueue({
              text: key,
              targetLang: lng,
              namespace: ns,
              key: key
            });
            
            if (process.env.NODE_ENV === 'development') {
              console.log(`[i18n] Added to translation queue: [${lng}] ${ns}:${key}`);
            }
          }
        }
      },
      interpolation: {
        escapeValue: false,
        formatSeparator: ',',
        format: function(value, format, lng) {
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
    // Save original send function
    const originalSend = res.send;
    
    res.send = function(body) {
      // Only process HTML responses
      if (typeof body === 'string' && 
          (res.get('Content-Type') || '').includes('text/html') || 
          body.includes('<!DOCTYPE html>') ||
          body.includes('<html>')) {
        
        // Get language from request (set by i18next-http-middleware)
        const language = req.language || defaultLanguage;
        
        if (language !== defaultLanguage) {
          // Replace data-i18n tags with translated text if available
          body = body.replace(/<([^>]+)\s+data-i18n="([^"]+)"([^>]*)>([^<]*)<\/([^>]+)>/g, (match, tag1, key, attrs, content, tag2) => {
            // Try to get translation
            const translation = req.i18n.t(key);
            
            // If translation equals key (not found), leave as is for frontend to handle
            if (translation === key) {
              return match; // Keep original tag for frontend retry
            }
            
            // Replace tag content with translation
            return `<${tag1}${attrs}>${translation}</${tag2}>`;
          });
          
          // Process data-i18n-attr attributes
          body = body.replace(/<([^>]+)\s+data-i18n-attr=['"]([^'"]+)['"]([^>]*)>/g, (match, tag, attrsJson, restAttrs) => {
            try {
              const attrsMap = JSON.parse(attrsJson);
              let newTag = `<${tag}${restAttrs}`;
              let allTranslated = true;
              
              for (const [attr, key] of Object.entries(attrsMap)) {
                const translation = req.i18n.t(key);
                
                // Get current attribute value if present
                const attrValueMatch = match.match(new RegExp(`${attr}="([^"]+)"`));
                const currentValue = attrValueMatch ? attrValueMatch[1] : '';
                
                // If translation differs from key, replace attribute value
                if (translation !== key) {
                  newTag = newTag.replace(
                    `${attr}="${currentValue}"`, 
                    `${attr}="${translation}"`
                  );
                } else {
                  allTranslated = false; // Mark that not all attributes are translated
                }
              }
              
              // If all attributes translated, remove data-i18n-attr
              if (allTranslated) {
                return newTag.replace(/\s+data-i18n-attr=['"][^'"]+['"]/, '') + '>';
              }
              
              // Otherwise keep the tag for frontend retry
              return match;
            } catch (e) {
              console.error('Error parsing data-i18n-attr:', e);
              return match;
            }
          });
        }
      }
      
      // Call original send function
      return originalSend.call(this, body);
    };
    
    next();
  };
  
  // Combine i18next middleware with our tag processor
  return [i18nextMiddleware.handle(i18next), tagProcessor];
}

module.exports = initI18n;