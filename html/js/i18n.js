// html/js/new-i18n.js - New i18next-based implementation
import i18next from 'i18next';
import LanguageDetector from 'i18next-browser-languagedetector';

/**
 * Helper function to get language from meta tag
 * @returns {string} The detected language code or null if not found
 */
function getLangFromMeta() {
  const metaTag = document.querySelector('meta[name="app-language"]');
  return metaTag ? metaTag.content : null;
}

/**
 * Asynchronously fetch translation from API when key is missing
 * @param {string} lang - Target language
 * @param {string} ns - Namespace
 * @param {string} key - Translation key
 * @param {string} fallbackValue - Original text to translate
 * @returns {Promise<string>} - Translated text
 */
async function fetchTranslation(lang, ns, key, fallbackValue) {
  if (!fallbackValue || lang === (process.env.DEFAULT_LANGUAGE || 'en')) {
    return fallbackValue;
  }
  
  try {
    const response = await fetch('/api/translate', {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'app-language': lang
      },
      body: JSON.stringify({
        text: fallbackValue,
        targetLang: lang,
        context: ns,
        background: true // Use background processing for smoother UX
      })
    });
    
    if (!response.ok) {
      console.warn(`Translation request failed: ${response.status}`);
      return fallbackValue;
    }
    
    const data = await response.json();
    return data.translated || fallbackValue;
  } catch (error) {
    console.error('Translation fetch error:', error);
    return fallbackValue;
  }
}

/**
 * Process untranslated elements in the DOM after page load
 */
async function processUntranslatedElements() {
  const lang = i18next.language;
  if (lang === (process.env.DEFAULT_LANGUAGE || 'en')) {
    return; // Skip for default language
  }
  
  // Find elements with i18n attributes
  const elementsToTranslate = [];
  
  // 1. Standard data-i18n attributes
  document.querySelectorAll('[data-i18n]').forEach(el => {
    const key = el.getAttribute('data-i18n');
    const content = el.textContent.trim();
    
    // Check if content looks untranslated (matches key or its last part)
    const keyParts = key.split(':');
    const shortKey = keyParts[keyParts.length - 1];
    const keyLastPart = shortKey.split('.').pop();
    
    if (content === '' || content === key || content === shortKey || content === keyLastPart) {
      elementsToTranslate.push({
        element: el,
        text: content || key,
        key: key,
        type: 'standard'
      });
    }
  });
  
  // 2. Attribute translations
  document.querySelectorAll('[data-i18n-attr]').forEach(el => {
    try {
      const attrsMap = JSON.parse(el.getAttribute('data-i18n-attr'));
      
      for (const [attr, key] of Object.entries(attrsMap)) {
        const attrValue = el.getAttribute(attr) || '';
        const keyParts = key.split(':');
        const shortKey = keyParts[keyParts.length - 1];
        
        if (attrValue === '' || attrValue === key || attrValue === shortKey) {
          elementsToTranslate.push({
            element: el,
            text: attrValue || key,
            key: key,
            type: 'attr',
            attrName: attr
          });
        }
      }
    } catch (e) {
      console.error('Error parsing data-i18n-attr:', e);
    }
  });
  
  // 3. HTML content translations
  document.querySelectorAll('[data-i18n-html]').forEach(el => {
    const key = el.getAttribute('data-i18n-html');
    const content = el.innerHTML.trim();
    
    const keyParts = key.split(':');
    const shortKey = keyParts[keyParts.length - 1];
    const keyLastPart = shortKey.split('.').pop();
    
    if (content === '' || content === key || content === shortKey || content === keyLastPart) {
      elementsToTranslate.push({
        element: el,
        text: content || key,
        key: key,
        type: 'html'
      });
    }
  });
  
  // Process elements in batches if there are many
  if (elementsToTranslate.length > 0) {
    console.log(`Found ${elementsToTranslate.length} untranslated elements to process`);
    
    // Group by type for more efficient API calls
    const textsByKey = {};
    
    // Group texts by their keys
    elementsToTranslate.forEach(item => {
      if (!textsByKey[item.key]) {
        textsByKey[item.key] = {
          text: item.text,
          elements: []
        };
      }
      textsByKey[item.key].elements.push(item);
    });
    
    // Process in smaller batches to avoid overloading API
    const batchSize = 20;
    const keys = Object.keys(textsByKey);
    
    for (let i = 0; i < keys.length; i += batchSize) {
      const batchKeys = keys.slice(i, i + batchSize);
      const batchTexts = batchKeys.map(key => textsByKey[key].text);
      
      try {
        // Use batch translation API
        const response = await fetch('/api/translate-batch', {
          method: 'POST',
          headers: {
            'Content-Type': 'application/json',
            'app-language': lang
          },
          body: JSON.stringify({
            texts: batchTexts,
            targetLang: lang,
            background: false // We want immediate results for DOM updates
          })
        });
        
        if (response.ok) {
          const data = await response.json();
          if (data.translations && data.translations.length === batchTexts.length) {
            // Update DOM with translations
            batchKeys.forEach((key, index) => {
              const translated = data.translations[index];
              const originalText = textsByKey[key].text;
              
              if (translated && translated !== originalText) {
                textsByKey[key].elements.forEach(item => {
                  if (item.type === 'standard') {
                    item.element.textContent = translated;
                  } else if (item.type === 'attr') {
                    item.element.setAttribute(item.attrName, translated);
                  } else if (item.type === 'html') {
                    item.element.innerHTML = translated;
                  }
                });
              }
            });
          }
        } else {
          console.warn('Batch translation request failed:', response.status);
        }
      } catch (error) {
        console.error('Batch translation error:', error);
      }
    }
  }
}

/**
 * Initialize i18next
 */
async function initI18next() {
  // Get initial language from meta tag if available
  const initialLang = getLangFromMeta() || 
    (document.documentElement.lang || (window.APP_CONFIG?.DEFAULT_LANGUAGE || 'en'));
  
  // Configure i18next
  await i18next
    .use(LanguageDetector)
    .init({
      lng: initialLang,
      fallbackLng: false, // Don't use fallback to show missing keys
      debug: window.APP_CONFIG?.NODE_ENV === 'development',
      
      // Empty resources - we'll use API for missing translations
      resources: {},
      
      // Language detection configuration
      detection: {
        order: ['htmlTag', 'querystring', 'cookie', 'navigator'],
        lookupQuerystring: 'lang',
        lookupCookie: 'i18next',
        lookupFromPathIndex: 0,
        lookupFromSubdomainIndex: 0,
        caches: ['cookie'],
        cookieExpirationDate: new Date(Date.now() + 365 * 24 * 60 * 60 * 1000), // 1 year
        htmlTag: document.documentElement
      },
      
      // Missing key handling
      saveMissing: true,
      missingKeyHandler: async (lngs, ns, key, fallbackValue) => {
        console.log(`Missing translation: [${lngs}] ${ns}:${key}`);
        
        // Skip for default language
        if (Array.isArray(lngs) && lngs[0] === (process.env.DEFAULT_LANGUAGE || 'en')) return;
        
        const lang = Array.isArray(lngs) ? lngs[0] : lngs;
        const translation = await fetchTranslation(lang, ns, key, fallbackValue);
        
        // Add translation to i18next store
        if (translation && translation !== fallbackValue) {
          const resources = {};
          if (!resources[lang]) resources[lang] = {};
          if (!resources[lang][ns]) resources[lang][ns] = {};
          resources[lang][ns][key] = translation;
          
          i18next.addResourceBundle(lang, ns, { [key]: translation }, true, true);
        }
      },
      
      // Interpolation settings
      interpolation: {
        escapeValue: false, // React already does escaping
        format: function(value, format, lng) {
          if (format === 'uppercase') return value.toUpperCase();
          return value;
        }
      }
    });
  
  // Set HTML attributes based on language
  document.documentElement.lang = i18next.language;
  
  // Set RTL direction for RTL languages
  if (['ar', 'he'].includes(i18next.language)) {
    document.documentElement.dir = 'rtl';
  } else {
    document.documentElement.dir = 'ltr';
  }
  
  // Process untranslated elements after initialization
  await processUntranslatedElements();
  
  // Set window.language for backward compatibility
  window.language = i18next.language;
  
  // Also set app-language header for future fetch requests (backward compatibility)
  const originalFetch = window.fetch;
  window.fetch = function(url, options = {}) {
    if (!options.headers) {
      options.headers = {};
    }
    
    // Add app-language header
    if (typeof options.headers.set === 'function') {
      options.headers.set('app-language', i18next.language);
    } else {
      options.headers['app-language'] = i18next.language;
    }
    
    return originalFetch.call(this, url, options);
  };
  
  // Backward compatibility - fire event
  document.dispatchEvent(new CustomEvent('i18n:initialized', {
    detail: { language: i18next.language }
  }));
  
  return i18next;
}

/**
 * Change the current language
 * @param {string} lang - Language code to switch to
 */
function changeLanguage(lang) {
  if (lang === i18next.language) return;
  
  // Set cookie for language persistence
  document.cookie = `i18next=${lang}; path=/; max-age=${60 * 60 * 24 * 365}`;
  try {
    localStorage.setItem('i18nextLng', lang);
  } catch (e) {
    console.warn("Failed to save language to localStorage", e);
  }
  
  // Reload page with language parameter
  const url = new URL(window.location.href);
  url.searchParams.set('lang', lang);
  url.searchParams.set('i18n_cb', Date.now()); // Cache busting
  window.location.href = url.toString();
}

/**
 * Update translations for dynamically added elements
 * @param {HTMLElement} element - Root element to translate
 */
function updateDynamicElement(element) {
  if (!element || i18next.language === (process.env.DEFAULT_LANGUAGE || 'en')) {
    return Promise.resolve();
  }
  
  // Process all data-i18n elements
  element.querySelectorAll('[data-i18n]').forEach(el => {
    const key = el.getAttribute('data-i18n');
    const defaultValue = el.textContent;
    
    // Use the defaultValue as fallback and for missing key handling
    const translation = i18next.t(key, { defaultValue });
    
    if (translation !== key) {
      el.textContent = translation;
    }
  });
  
  // Process data-i18n-attr attributes
  element.querySelectorAll('[data-i18n-attr]').forEach(el => {
    try {
      const attrsMap = JSON.parse(el.getAttribute('data-i18n-attr'));
      
      for (const [attr, key] of Object.entries(attrsMap)) {
        const defaultValue = el.getAttribute(attr) || '';
        
        // Use defaultValue for missing key handling
        const translation = i18next.t(key, { defaultValue });
        
        if (translation !== key) {
          el.setAttribute(attr, translation);
        }
      }
    } catch (e) {
      console.error('Error parsing data-i18n-attr:', e);
    }
  });
  
  // Process data-i18n-html attributes
  element.querySelectorAll('[data-i18n-html]').forEach(el => {
    const key = el.getAttribute('data-i18n-html');
    const defaultValue = el.innerHTML;
    
    // Use defaultValue for missing key handling with HTML interpolation
    const translation = i18next.t(key, { 
      defaultValue,
      interpolation: { escapeValue: false }
    });
    
    if (translation !== key) {
      el.innerHTML = translation;
    }
  });
  
  return Promise.resolve();
}

// Initialize i18next when the DOM is ready
if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', initI18next);
} else {
  initI18next();
}

// Set up a mutation observer to translate dynamically added elements
if (window.MutationObserver) {
  const observer = new MutationObserver(mutations => {
    for (const mutation of mutations) {
      if (mutation.type === 'childList' && mutation.addedNodes.length) {
        for (const node of mutation.addedNodes) {
          if (node.nodeType === Node.ELEMENT_NODE) {
            updateDynamicElement(node);
          }
        }
      }
    }
  });
  
  // Start observing once DOM is loaded
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', () => {
      observer.observe(document.body, {
        childList: true,
        subtree: true
      });
    });
  } else {
    observer.observe(document.body, {
      childList: true,
      subtree: true
    });
  }
}

// Backward compatibility API
window.i18n = {
  // Main API
  init: initI18next,
  t: (key, options) => i18next.t(key, options),
  changeLanguage,
  getCurrentLanguage: () => i18next.language,
  updatePageTranslations: processUntranslatedElements,
  
  // Legacy compatibility methods
  translateDynamicElement: updateDynamicElement,
  isInitialized: () => !!i18next.isInitialized,
  syncLanguageMasks: () => {
    // Update SVG language masks for backward compatibility
    try {
      // Close all language masks
      const supportedLanguages = [
        'de', 'en', 'fr', 'it', 'es', 'pt', 'nl', 'da', 'sv', 'fi',
        'el', 'cs', 'pl', 'hu', 'sk', 'sl', 'et', 'lv', 'lt', 'ro',
        'bg', 'hr', 'ga', 'mt', 'ru', 'tr', 'ar', 'zh', 'uk', 'sr', 
        'he', 'ko', 'ja'
      ];
      
      supportedLanguages.forEach(lang => {
        const maskElement = document.getElementById(`${lang}Mask`);
        if (maskElement) {
          maskElement.setAttribute("mask", "url(#maskClose)");
        }
      });
      
      // Open current language mask
      const currentMask = document.getElementById(`${i18next.language}Mask`);
      if (currentMask) {
        currentMask.setAttribute("mask", "url(#maskOpen)");
      }
    } catch (e) {
      console.warn("Failed to synchronize SVG language masks:", e);
    }
  }
};

// Export i18next and helper functions
export {
  i18next,
  initI18next,
  changeLanguage,
  processUntranslatedElements,
  updateDynamicElement
};