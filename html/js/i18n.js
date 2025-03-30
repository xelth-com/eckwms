// Modified i18n.js with RMA form compatibility

(function() {
  // Default language fallback
  const defaultLanguage = 'en';
  
  // Store translations cache
  const translationsCache = {};
  
  // Enhanced tracking of pending translations
  const pendingTranslations = {};
  
  // Types of pending elements
  const PENDING_TYPES = {
    TEXT: 'text',
    ATTR: 'attribute',
    HTML: 'html'
  };
  
  // Track initialization state
  let isInitialized = false;
  
  // RMA compatibility: Add event dispatching for initialization
  let rmaCompatibilityMode = true;
  
  // Verbose logging control
  const VERBOSE_LOGGING = true;
  
  /**
   * Enhanced logging function that can be easily toggled
   */
  function log(...args) {
    if (VERBOSE_LOGGING) {
      console.log('[i18n]', ...args);
    }
  }
  
  /**
   * Error logging function that always logs
   */
  function logError(...args) {
    console.error('[i18n ERROR]', ...args);
  }
  
  /**
   * Helper function to get language from meta tag (server-provided)
   */
  function getLangFromMeta() {
    const metaTag = document.querySelector('meta[name="app-language"]');
    return metaTag ? metaTag.content : null;
  }
  
  /**
   * Helper function to get current language from various sources
   */
  function getCurrentLanguage() {
    // Meta tag has highest priority (server-provided)
    const metaLang = getLangFromMeta();
    if (metaLang) return metaLang;
    
    // Check HTML lang attribute
    const htmlLang = document.documentElement.lang;
    if (htmlLang) return htmlLang;
    
    // Check cookie
    const cookieMatch = document.cookie.match(/i18next=([^;]+)/);
    if (cookieMatch) return cookieMatch[1];
    
    // Default language fallback
    return defaultLanguage;
  }
  
  /**
   * Change the current language
   */
  function setLanguage(langPos) {
    // Current language
    const previousLanguage = getCurrentLanguage();
    
    // Parse language from button or direct string
    let newLanguage;
    if (langPos.slice(0, 4) === "lang") {
      newLanguage = document.getElementById(langPos).getAttribute("href").slice(1);
    } else {
      newLanguage = langPos;
    }
    
    log(`Changing language from ${previousLanguage} to ${newLanguage}`);
    
    if (newLanguage === previousLanguage) {
      log('Language is already set to', newLanguage, '- skipping change');
      return;
    }
    
    // Visual updates for SVG masks
    try {
      document.getElementById(`${previousLanguage}Mask`).setAttribute("mask", "url(#maskClose)");
    } catch (e) { /* Silent catch */ }
    
    // Store language preference in cookie and localStorage
    document.cookie = `i18next=${newLanguage}; path=/; max-age=${60 * 60 * 24 * 365}`;
    try {
      localStorage.setItem('i18nextLng', newLanguage);
    } catch (e) { /* Silent catch */ }
    
    // Add cache busting parameter to URL and reload
    const cacheBuster = Date.now();
    const url = new URL(window.location.href);
    url.searchParams.set('i18n_cb', cacheBuster);
    url.searchParams.set('lang', newLanguage);
    
    window.location.href = url.toString();
  }
  
  /**
   * Initialize i18n with meta tag prioritization
   */
  function init() {
    // If already initialized, skip
    if (isInitialized) {
      log('Already initialized, skipping');
      return Promise.resolve();
    }
    
    // Mark as initialized
    isInitialized = true;
    
    // Get language from meta tag (server-provided)
    const language = getCurrentLanguage();
    
    log(`Initializing with language: ${language}`);
    
    // Set language attributes
    document.documentElement.lang = language;
    
    // Set RTL direction for RTL languages
    if (['ar', 'he'].includes(language)) {
      document.documentElement.dir = 'rtl';
    } else {
      document.documentElement.dir = 'ltr';
    }
    
    // Set global language
    window.language = language;
    
    // Update language masks in SVG
    syncLanguageMasks();
    
    // Setup fetch interception for language headers
    setupFetchInterception();
    
    // Translate existing elements
    if (language !== defaultLanguage) {
      log(`Language is not default (${defaultLanguage}), translating page elements`);
      translatePageElements();
    } else {
      log(`Language is default (${defaultLanguage}), skipping translation`);
    }
    
    // Setup mutation observer for dynamic content
    setupMutationObserver();
    
    // RMA compatibility: Fire initialization event
    if (rmaCompatibilityMode) {
      log('Dispatching i18n:initialized event for RMA compatibility');
      setTimeout(() => {
        const initEvent = new CustomEvent('i18n:initialized', {
          detail: { language }
        });
        document.dispatchEvent(initEvent);
      }, 10);
    }
    
    log('Initialization complete');
    return Promise.resolve(language);
  }
  
  // Initialize when DOM is ready
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', function() {
      if (window.translationUtils) {
        init();
      }
    });
  } else {
    if (window.translationUtils) {
      init();
    }
  }
  
  /**
   * Setup fetch interception to add language headers to all requests
   */
  function setupFetchInterception() {
    log('Setting up fetch interception for language headers');
    const originalFetch = window.fetch;
    window.fetch = function(url, options = {}) {
      if (!options.headers) {
        options.headers = {};
      }
      
      const currentLang = getCurrentLanguage();
      
      // Add app-language header to all fetch requests
      if (typeof options.headers.set === 'function') {
        options.headers.set('app-language', currentLang);
      } else {
        options.headers['app-language'] = currentLang;
      }
      
      return originalFetch.call(this, url, options);
    };
  }
  
  /**
   * Setup mutation observer to translate dynamically added elements
   */
  function setupMutationObserver() {
    if (!window.MutationObserver) {
      log('MutationObserver not supported, skipping dynamic translation setup');
      return;
    }
    
    log('Setting up MutationObserver for dynamic content');
    const observer = new MutationObserver(mutations => {
      let elementsToTranslate = 0;
      
      for (const mutation of mutations) {
        if (mutation.type === 'childList' && mutation.addedNodes.length) {
          for (const node of mutation.addedNodes) {
            if (node.nodeType === Node.ELEMENT_NODE) {
              if (needsTranslation(node)) {
                elementsToTranslate++;
                translateDynamicElement(node);
              }
            }
          }
        }
      }
      
      if (elementsToTranslate > 0) {
        log(`Translated ${elementsToTranslate} dynamically added elements`);
      }
    });
    
    // Start observing
    observer.observe(document.body, {
      childList: true,
      subtree: true
    });
  }
  
  /**
   * Check if an element needs translation
   */
  function needsTranslation(element) {
    // Check if element itself has translation attributes
    if (element.hasAttribute('data-i18n') || 
        element.hasAttribute('data-i18n-attr') || 
        element.hasAttribute('data-i18n-html')) {
      return true;
    }
    
    // Check if any children have translation attributes
    return element.querySelector('[data-i18n], [data-i18n-attr], [data-i18n-html]') !== null;
  }
  
  /**
   * Translate an element and its children
   */
  function translateDynamicElement(element) {
    if (getCurrentLanguage() === defaultLanguage) return;
    
    // Process standard translations
    const standardElements = element.querySelectorAll('[data-i18n]');
    if (standardElements.length > 0) {
      log(`Found ${standardElements.length} elements with data-i18n in dynamic content`);
      standardElements.forEach(processElementTranslation);
    }
    
    if (element.hasAttribute('data-i18n')) {
      processElementTranslation(element);
    }
    
    // Process attribute translations
    const attrElements = element.querySelectorAll('[data-i18n-attr]');
    if (attrElements.length > 0) {
      log(`Found ${attrElements.length} elements with data-i18n-attr in dynamic content`);
      attrElements.forEach(processAttrTranslation);
    }
    
    if (element.hasAttribute('data-i18n-attr')) {
      processAttrTranslation(element);
    }
    
    // Process HTML translations
    const htmlElements = element.querySelectorAll('[data-i18n-html]');
    if (htmlElements.length > 0) {
      log(`Found ${htmlElements.length} elements with data-i18n-html in dynamic content`);
      htmlElements.forEach(processHtmlTranslation);
    }
    
    if (element.hasAttribute('data-i18n-html')) {
      processHtmlTranslation(element);
    }
  }
  
  /**
   * Process element text translation
   */
  function processElementTranslation(element) {
    const key = element.getAttribute('data-i18n');
    if (!key) return;
    
    // RMA compatibility: Add namespace prefix if needed
    let fullKey = key;
    if (!key.includes(':') && getCurrentLanguage() !== 'en') {
      fullKey = 'rma:' + key;
    }
    
    // Important: Get original text *before* emptying it for immediate display
    const originalText = element.textContent.trim();
    
    // RMA compatibility: Set the same text immediately (don't empty it)
    // This keeps the original text visible during translation
    
    // Add handling for additional options
    let options = {};
    try {
      const optionsAttr = element.getAttribute('data-i18n-options');
      if (optionsAttr) {
        options = JSON.parse(optionsAttr);
        log(`Processing element with options: ${optionsAttr}`, element);
      }
    } catch (error) {
      logError('Error parsing data-i18n-options:', error);
    }
    
    // Add defaultValue to options
    options.defaultValue = originalText;
    
    log(`Translating element with key: "${fullKey}", text: "${originalText.substring(0, 30)}${originalText.length > 30 ? '...' : ''}"`);
    
    // Information about element for adding to waiting list
    const pendingInfo = {
      element: element,
      type: PENDING_TYPES.TEXT,
      key: fullKey
    };
    
    // Use API translation with key, text and options
    fetchTranslation(fullKey, originalText, options, pendingInfo).then(translation => {
      if (translation) {
        log(`Applied translation for "${fullKey}": "${translation.substring(0, 30)}${translation.length > 30 ? '...' : ''}"`);
        element.textContent = translation;
      } else {
        log(`No translation applied for "${fullKey}", using original text`);
      }
    }).catch(err => {
      logError(`Failed to fetch translation for key "${fullKey}":`, err);
    });
  }
  
  /**
   * Process element attribute translations
   */
  function processAttrTranslation(element) {
    try {
      const attrsJson = element.getAttribute('data-i18n-attr');
      if (!attrsJson) return;
      
      const attrs = JSON.parse(attrsJson);
      log(`Processing attribute translations: ${attrsJson}`, element);
      
      for (const [attr, key] of Object.entries(attrs)) {
        // RMA compatibility: Add namespace prefix if needed
        let fullKey = key;
        if (!key.includes(':') && getCurrentLanguage() !== 'en') {
          fullKey = 'rma:' + key;
        }
        
        const originalValue = element.getAttribute(attr) || '';
        
        log(`Translating attribute "${attr}" with key "${fullKey}", value: "${originalValue}"`);
        
        // Information about element for adding to waiting list
        const pendingInfo = {
          element: element,
          type: PENDING_TYPES.ATTR,
          attr: attr,
          key: fullKey
        };
        
        // Use API translation for this key/value
        fetchTranslation(fullKey, originalValue, {}, pendingInfo).then(translation => {
          if (translation && translation !== originalValue) {
            log(`Applied translation for attribute "${attr}": "${translation}"`);
            element.setAttribute(attr, translation);
          } else {
            log(`No translation applied for attribute "${attr}", using original value`);
          }
        }).catch(err => {
          logError(`Failed to fetch translation for attribute "${attr}" with key "${fullKey}":`, err);
        });
      }
    } catch (error) {
      logError('Error processing attribute translation:', error);
    }
  }
  
  /**
   * Process HTML content translations
   */
  function processHtmlTranslation(element) {
    const key = element.getAttribute('data-i18n-html');
    if (!key) return;
    
    // RMA compatibility: Add namespace prefix if needed
    let fullKey = key;
    if (!key.includes(':') && getCurrentLanguage() !== 'en') {
      fullKey = 'rma:' + key;
    }
    
    const originalHtml = element.innerHTML.trim();
    
    log(`Translating HTML content with key: "${fullKey}", length: ${originalHtml.length} chars`);
    
    // Information about element for adding to waiting list
    const pendingInfo = {
      element: element,
      type: PENDING_TYPES.HTML,
      key: fullKey
    };
    
    // Use API translation for this key/html
    fetchTranslation(fullKey, originalHtml, {}, pendingInfo).then(translation => {
      if (translation && translation !== originalHtml) {
        log(`Applied HTML translation for "${fullKey}", length: ${translation.length} chars`);
        element.innerHTML = translation;
      } else {
        log(`No HTML translation applied for "${fullKey}", using original content`);
      }
    }).catch(err => {
      logError(`Failed to fetch HTML translation for key "${fullKey}":`, err);
    });
  }
  
  /**
   * Translate all page elements
   */
  function translatePageElements() {
    const language = getCurrentLanguage();
    log(`Starting page translation for language: ${language}`);
    
    // Skip if default language
    if (language === defaultLanguage) {
      log('Using default language, skipping translation');
      return;
    }
    
    // Process standard translations
    const standardElements = document.querySelectorAll('[data-i18n]');
    log(`Found ${standardElements.length} elements with data-i18n`);
    standardElements.forEach(processElementTranslation);
    
    // Process attribute translations
    const attrElements = document.querySelectorAll('[data-i18n-attr]');
    log(`Found ${attrElements.length} elements with data-i18n-attr`);
    attrElements.forEach(processAttrTranslation);
    
    // Process HTML translations
    const htmlElements = document.querySelectorAll('[data-i18n-html]');
    log(`Found ${htmlElements.length} elements with data-i18n-html`);
    htmlElements.forEach(processHtmlTranslation);
    
    log('Page translation initiated, waiting for API responses');
  }
   
  /**
   * Fetch translation with improved cache key generation and pending request handling
   * - Modified to better handle the RMA form translation approach
   */
  async function fetchTranslation(key, defaultText, options = {}, pendingInfo = null) {
    const language = getCurrentLanguage();
    
    // Skip translation for default language
    if (language === defaultLanguage) {
      log(`Using default language (${defaultLanguage}), skipping translation fetch for "${key}"`);
      return Promise.resolve(defaultText);
    }
    
    // Extract namespace from key if present
    let namespace = 'common';
    let translationKey = key;
    
    if (key.includes(':')) {
      const parts = key.split(':');
      namespace = parts[0];
      translationKey = parts.slice(1).join(':');
    }
    
    // Use the same algorithm as server to generate cache key
    const cacheKey = window.translationUtils ? 
        window.translationUtils.generateTranslationKey(defaultText, language, namespace, options) :
        `${language}:${namespace}:${translationKey}`;
    
    // Add language prefix for client-side cache
    const fullCacheKey = `${language}:${cacheKey}`;
    
    log(`Translation request for key: "${key}" with cache key: "${fullCacheKey}"`);
    
    // Check client cache first
    if (translationsCache[fullCacheKey]) {
      log(`Found in client cache: "${fullCacheKey}"`);
      return Promise.resolve(translationsCache[fullCacheKey]);
    }
    
    // Add request to an already waiting one, if it exists
    if (pendingTranslations[fullCacheKey]) {
      log(`Translation already pending for "${fullCacheKey}", registering element for later update`);
      
      // If element info is provided, register it for updating later
      if (pendingInfo) {
        // Create elements array if it doesn't exist
        if (!pendingTranslations[fullCacheKey].elements) {
          pendingTranslations[fullCacheKey].elements = [];
        }
        pendingTranslations[fullCacheKey].elements.push(pendingInfo);
      }
      
      // Return existing Promise to avoid creating duplicate requests
      return pendingTranslations[fullCacheKey].request;
    }
    
    // Create a new Promise for the request
    const translationPromise = new Promise(async (resolve, reject) => {
      try {
        log(`Making API request for translation: "${key}" in language: ${language}`);
        
        // RMA compatibility: First try to fetch from locales file
        try {
          const localeUrl = `/locales/${language}/${namespace}.json`;
          log(`Trying to fetch from locale file: ${localeUrl}`);
          
          const response = await fetch(localeUrl);
          if (response.ok) {
            const data = await response.json();
            
            // Find the translation in the nested structure
            let translation = navigateToKey(data, translationKey);
            
            if (translation) {
              log(`Found translation in locale file: "${translation}"`);
              
              // Apply parameter substitution
              if (options.count !== undefined && typeof translation === 'string') {
                translation = translation.replace(/\{\{count\}\}/g, options.count);
              }
              
              // Cache the translation
              translationsCache[fullCacheKey] = translation;
              
              // Update all pending elements
              if (pendingTranslations[fullCacheKey] && pendingTranslations[fullCacheKey].elements) {
                updateAllPendingElements(pendingTranslations[fullCacheKey].elements, translation);
              }
              
              // Update all elements with this key
              updateAllElementsWithKey(key, translation);
              
              // Resolve the Promise
              resolve(translation);
              return translation;
            }
          }
        } catch (error) {
          log(`Error fetching from locale file: ${error.message}`);
          // Continue with API translation on error
        }
        
        // If locale file translation failed, use API
        const response = await fetch(`/api/translate`, {
          method: 'POST',
          headers: {
            'Content-Type': 'application/json',
            'app-language': language
          },
          body: JSON.stringify({
            text: defaultText,
            targetLang: language,
            context: namespace,
            key: translationKey,
            defaultValue: defaultText,
            background: false,
            options: options
          })
        });
        
        const data = await response.json();
        
        log(`API response for "${key}": status=${response.status}, fromCache=${data.fromCache}, translated length=${data.translated?.length}`);
        
        // Handle "pending" status with retry
        if (response.status === 202 && data.status === 'pending') {
          const retryAfter = parseInt(response.headers.get('Retry-After') || '3', 10);
          
          log(`Translation pending for "${key}", will retry after ${retryAfter} seconds`);
          
          // Schedule retry with the server's suggested delay
          setTimeout(() => {
            log(`Retrying translation for "${key}" after delay`);
            
            // Save pending elements for later updating
            const pendingElements = pendingTranslations[fullCacheKey]?.elements || [];
            
            // Remove from pending to allow new request
            delete pendingTranslations[fullCacheKey];
            
            // Re-request the translation
            fetchTranslation(key, defaultText, options).then(translation => {
              log(`Retry successful for "${key}", updating elements`);
              
              // Update all pending elements
              updateAllPendingElements(pendingElements, translation);
              
              // Resolve the Promise with the obtained translation
              resolve(translation);
            }).catch(err => {
              logError(`Retry failed for "${key}":`, err);
              reject(err);
            });
          }, retryAfter * 1000);
          
          // Return default text for now
          return defaultText;
        }
        
        // Handle completed translation
        if (data.translated) {
          const translation = data.translated;
          log(`Received translation for "${key}": "${translation.substring(0, 30)}${translation.length > 30 ? '...' : ''}"`);
          
          // Cache the translation using the same key format as backend
          translationsCache[fullCacheKey] = translation;
          
          // Update all pending elements
          if (pendingTranslations[fullCacheKey] && pendingTranslations[fullCacheKey].elements) {
            updateAllPendingElements(pendingTranslations[fullCacheKey].elements, translation);
          }
          
          // Clear pending status
          delete pendingTranslations[fullCacheKey];
          
          // Also update all elements with this key
          updateAllElementsWithKey(key, translation);
          
          // Resolve Promise with the obtained translation
          resolve(translation);
          return translation;
        }
        
        // If we got here, no usable translation
        log(`No usable translation for "${key}", using original text`);
        delete pendingTranslations[fullCacheKey];
        resolve(defaultText);
        return defaultText;
      } catch (error) {
        logError(`API error when translating "${key}":`, error);
        delete pendingTranslations[fullCacheKey];
        
        // RMA compatibility: Don't reject, but return the default text to avoid breaking the UI
        resolve(defaultText);
        return defaultText;
      }
    });
    
    // Save the Promise and element info in pending list
    pendingTranslations[fullCacheKey] = {
      request: translationPromise,
      elements: pendingInfo ? [pendingInfo] : []
    };
    
    return translationPromise;
  }
  
  /**
   * Helper function to navigate a nested structure by a dot-separated key
   * E.g., "device.title" would navigate to obj.device.title
   */
  function navigateToKey(obj, key) {
    if (!obj || !key) return null;
    
    const parts = key.split('.');
    let current = obj;
    
    for (const part of parts) {
      if (current[part] === undefined) {
        return null;
      }
      current = current[part];
    }
    
    return current;
  }
  
  /**
   * Update all pending elements waiting for a translation
   */
  function updateAllPendingElements(pendingElements, translation) {
    if (!pendingElements || !pendingElements.length) {
      return;
    }
    
    log(`Updating ${pendingElements.length} pending elements with new translation`);
    
    pendingElements.forEach(info => {
      if (!info || !info.element) return;
      
      // Apply appropriate update based on element type
      switch (info.type) {
        case PENDING_TYPES.TEXT:
          // Update text content
          info.element.textContent = translation;
          log(`Updated text element with translation, key: ${info.key}`);
          break;
          
        case PENDING_TYPES.ATTR:
          // Update attribute
          if (info.attr) {
            info.element.setAttribute(info.attr, translation);
            log(`Updated attribute ${info.attr} with translation, key: ${info.key}`);
          }
          break;
          
        case PENDING_TYPES.HTML:
          // Update HTML content
          info.element.innerHTML = translation;
          log(`Updated HTML element with translation, key: ${info.key}`);
          break;
          
        default:
          logError(`Unknown pending element type: ${info.type}`);
      }
    });
  }
  
  /**
   * Update all elements with a specific translation key
   */
  function updateAllElementsWithKey(key, translation) {
    log(`Updating all elements with key "${key}" to new translation`);
    
    // RMA compatibility: Handle both prefixed and non-prefixed keys
    const keyWithoutPrefix = key.includes(':') ? key.split(':')[1] : key;
    
    let updateCount = 0;
    
    // Update text content translations
    document.querySelectorAll(`[data-i18n="${key}"], [data-i18n="${keyWithoutPrefix}"]`).forEach(element => {
      element.textContent = translation;
      updateCount++;
    });
    
    // Update HTML translations
    document.querySelectorAll(`[data-i18n-html="${key}"], [data-i18n-html="${keyWithoutPrefix}"]`).forEach(element => {
      element.innerHTML = translation;
      updateCount++;
    });
    
    // Update attribute translations
    document.querySelectorAll('[data-i18n-attr]').forEach(element => {
      try {
        const attrsJson = element.getAttribute('data-i18n-attr');
        if (!attrsJson) return;
        
        const attrs = JSON.parse(attrsJson);
        for (const [attr, attrKey] of Object.entries(attrs)) {
          if (attrKey === key || attrKey === keyWithoutPrefix) {
            element.setAttribute(attr, translation);
            updateCount++;
          }
        }
      } catch (error) {
        logError('Error processing attribute translation update:', error);
      }
    });
    
    log(`Updated ${updateCount} elements with key "${key}"`);
  }
  
  /**
   * Update SVG language masks to show active language
   */
  function syncLanguageMasks() {
    try {
      const currentLang = getCurrentLanguage();
      log(`Synchronizing language masks for ${currentLang}`);
      
      // Close all masks
      const supportedLangs = ['de', 'en', 'fr', 'it', 'es', 'pt', 'nl', 'da', 'sv', 'fi', 
                             'el', 'cs', 'pl', 'hu', 'sk', 'sl', 'et', 'lv', 'lt', 'ro',
                             'bg', 'hr', 'ga', 'mt', 'ru', 'tr', 'ar', 'zh', 'uk', 'sr', 
                             'he', 'ko', 'ja'];
      
      supportedLangs.forEach(lang => {
        const maskElement = document.getElementById(`${lang}Mask`);
        if (maskElement) {
          maskElement.setAttribute("mask", "url(#maskClose)");
        }
      });
      
      // Open current language mask
      const currentMask = document.getElementById(`${currentLang}Mask`);
      if (currentMask) {
        currentMask.setAttribute("mask", "url(#maskOpen)");
      }
    } catch (e) {
      logError("Error synchronizing language masks:", e);
    }
  }
  
  /**
   * Simple translation function - will use cache or default text
   * RMA compatibility: Adjusted to handle RMA form's approach
   */
  function t(key, options = {}) {
    const defaultValue = options.defaultValue || key;
    const language = getCurrentLanguage();
    
    // Skip translation for default language
    if (language === defaultLanguage) {
      return defaultValue;
    }
    
    // Extract namespace
    let namespace = 'common';
    let translationKey = key;
    
    if (key.includes(':')) {
      const parts = key.split(':');
      namespace = parts[0];
      translationKey = parts.slice(1).join(':');
    }
    
    // Support both prefixing styles
    const fullKey = key.includes(':') ? key : `rma:${key}`;
    
    // Check cache and return immediately if found
    const cacheKey = window.translationUtils ? 
        window.translationUtils.generateTranslationKey(defaultValue, language, namespace, options) :
        `${language}:${namespace}:${translationKey}`;
    const fullCacheKey = `${language}:${cacheKey}`;
    
    // Check cache and return immediately if found
    if (translationsCache[fullCacheKey]) {
      return translationsCache[fullCacheKey];
    }
    
    // Not in cache, schedule async fetch for future use
    if (!pendingTranslations[fullCacheKey]) {
      log(`Scheduling async fetch for "${fullKey}"`);
      
      // Initialize pending entry properly
      pendingTranslations[fullCacheKey] = {
        request: null,
        elements: []
      };
      
      fetchTranslation(fullKey, defaultValue, options)
        .then(translation => {
          translationsCache[fullCacheKey] = translation;
          delete pendingTranslations[fullCacheKey];
          
          // If the translation is different, update DOM elements
          if (translation !== defaultValue) {
            updateAllElementsWithKey(fullKey, translation);
          }
        })
        .catch(() => {
          delete pendingTranslations[fullCacheKey];
        });
    }
    
    // Return default for now
    return defaultValue;
  }
  
  // Export public API - RMA compatibility: Make sure all needed functions are exposed
  window.i18n = {
    init,
    t,
    getCurrentLanguage,
    changeLanguage: setLanguage,
    translateDynamicElement,
    updatePageTranslations: translatePageElements,
    syncLanguageMasks,
    isInitialized: () => isInitialized,
    
    // RMA compatibility: Export additional functions required by RMA form
    setRmaCompatibilityMode: (mode) => { rmaCompatibilityMode = mode },
    forceTriggerInitEvent: () => {
      const event = new CustomEvent('i18n:initialized', {
        detail: { language: getCurrentLanguage() }
      });
      document.dispatchEvent(event);
    }
  };
})();