// html/js/i18n.js - Enhanced with improved logging and retry mechanisms
(function() {
  // Default language fallback
  const defaultLanguage = 'en';
  
  // Store translations cache
  const translationsCache = {};
  
  // Track pending translations to avoid duplicate requests
  const pendingTranslations = {};
  
  // Track initialization state
  let isInitialized = false;
  
  // Verbose logging control - enable in development
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
   * @returns {string} The detected language code or null if not found
   */
  function getLangFromMeta() {
    const metaTag = document.querySelector('meta[name="app-language"]');
    return metaTag ? metaTag.content : null;
  }
  
  /**
   * Helper function to get current language from various sources
   * with meta tag having highest priority
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
   * Change the current language - with cache busting URL parameter
   * @param {string} langPos - Language position or code
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
    url.searchParams.set('lang', newLanguage); // Explicitly set language parameter
    
    log(`Redirecting to ${url.toString()} with new language ${newLanguage}`);
    window.location.href = url.toString();
  }
  
  /**
   * Initialize i18n with meta tag prioritization
   */
  function init() {
    if (isInitialized) {
      log('Already initialized, skipping');
      return Promise.resolve();
    }
    
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
    
    // Clean URL from cache busting parameter after page load
    if (window.location.search.includes('i18n_cb')) {
      // Wait for page to fully load
      setTimeout(() => {
        // Use History API to clean URL without page reload
        const cleanUrl = new URL(window.location.href);
        cleanUrl.searchParams.delete('i18n_cb');
        window.history.replaceState({}, document.title, cleanUrl.toString());
        log('Removed cache busting parameter from URL');
      }, 2000); // 2 seconds delay to ensure page loaded
    }
    
    // Mark as initialized
    isInitialized = true;
    
    // Fire initialized event for other scripts
    document.dispatchEvent(new CustomEvent('i18n:initialized', {
      detail: { language }
    }));
    
    log('Initialization complete');
    return Promise.resolve(language);
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
      
      // Log translation-related API calls
      if (url.includes('/api/translate')) {
        log(`Making translation API request to ${url} with language: ${currentLang}`);
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
   * @param {HTMLElement} element - Element to check
   * @returns {boolean} - True if element needs translation
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
   * @param {HTMLElement} element - Element to translate
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
   * @param {HTMLElement} element - Element to process
   */
  function processElementTranslation(element) {
    const key = element.getAttribute('data-i18n');
    if (!key) return;
    
    const originalText = element.textContent.trim();
    
    // Add handling for additional options (including count)
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
    
    log(`Translating element with key: "${key}", text: "${originalText.substring(0, 30)}${originalText.length > 30 ? '...' : ''}"`);
    
    // Use API translation with key, text and options
    fetchTranslation(key, originalText, options).then(translation => {
      if (translation && translation !== originalText) {
        log(`Applied translation for "${key}": "${translation.substring(0, 30)}${translation.length > 30 ? '...' : ''}"`);
        element.textContent = translation;
      } else {
        log(`No translation applied for "${key}", using original text`);
      }
    }).catch(err => {
      logError(`Failed to fetch translation for key "${key}":`, err);
    });
  }
  
  /**
   * Process element attribute translations
   * @param {HTMLElement} element - Element with attribute translations
   */
  function processAttrTranslation(element) {
    try {
      const attrsJson = element.getAttribute('data-i18n-attr');
      if (!attrsJson) return;
      
      const attrs = JSON.parse(attrsJson);
      log(`Processing attribute translations: ${attrsJson}`, element);
      
      for (const [attr, key] of Object.entries(attrs)) {
        const originalValue = element.getAttribute(attr) || '';
        
        log(`Translating attribute "${attr}" with key "${key}", value: "${originalValue}"`);
        
        // Use API translation for this key/value
        fetchTranslation(key, originalValue).then(translation => {
          if (translation && translation !== originalValue) {
            log(`Applied translation for attribute "${attr}": "${translation}"`);
            element.setAttribute(attr, translation);
          } else {
            log(`No translation applied for attribute "${attr}", using original value`);
          }
        }).catch(err => {
          logError(`Failed to fetch translation for attribute "${attr}" with key "${key}":`, err);
        });
      }
    } catch (error) {
      logError('Error processing attribute translation:', error);
    }
  }
  
  /**
   * Process HTML content translations
   * @param {HTMLElement} element - Element with HTML content to translate
   */
  function processHtmlTranslation(element) {
    const key = element.getAttribute('data-i18n-html');
    if (!key) return;
    
    const originalHtml = element.innerHTML.trim();
    
    log(`Translating HTML content with key: "${key}", length: ${originalHtml.length} chars`);
    
    // Use API translation for this key/html
    fetchTranslation(key, originalHtml).then(translation => {
      if (translation && translation !== originalHtml) {
        log(`Applied HTML translation for "${key}", length: ${translation.length} chars`);
        element.innerHTML = translation;
      } else {
        log(`No HTML translation applied for "${key}", using original content`);
      }
    }).catch(err => {
      logError(`Failed to fetch HTML translation for key "${key}":`, err);
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
   * Fetch translation with improved pending handling and logging
   * @param {string} key - Translation key
   * @param {string} defaultText - Default text if no translation found
   * @param {object} options - Translation options (count, etc.)
   * @returns {Promise<string>} - Translation or default text
   */
  async function fetchTranslation(key, defaultText, options = {}) {
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
    
    // Create unique key for cache
    const optionsKey = Object.keys(options).length > 0 ? 
      `:${JSON.stringify(options)}` : '';
    
    const cacheKey = `${language}:${namespace}:${translationKey}${optionsKey}`;
    
    // Debugging info about translation request
    log(`Translation request for key: "${key}" (${cacheKey})`);
    
    // Check client cache first
    if (translationsCache[cacheKey]) {
      log(`Found in client cache: "${cacheKey}"`);
      return Promise.resolve(translationsCache[cacheKey]);
    }
    
    // Check if this translation is already pending
    if (pendingTranslations[cacheKey]) {
      log(`Translation already pending for "${cacheKey}", using original text for now`);
      return defaultText;
    }
    
    // Mark as pending to prevent duplicate requests
    pendingTranslations[cacheKey] = true;
    
    try {
      log(`Making API request for translation: "${key}" in language: ${language}`);
      
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
          background: false, // We want immediate response if possible
          options: options  // Add options to request
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
          
          // Remove from pending to allow new request
          delete pendingTranslations[cacheKey];
          
          // Re-request the translation
          fetchTranslation(key, defaultText, options).then(translation => {
            // Update elements with this key when translation arrives
            log(`Retry successful for "${key}", updating elements`);
            updateTranslatedElements(key, translation);
          }).catch(err => {
            logError(`Retry failed for "${key}":`, err);
          });
        }, retryAfter * 1000);
        
        // Return default text for now
        return defaultText;
      }
      
      // Handle completed translation
      if (data.translated) {
        log(`Received translation for "${key}": "${data.translated.substring(0, 30)}${data.translated.length > 30 ? '...' : ''}"`);
        
        // Cache the translation
        translationsCache[cacheKey] = data.translated;
        
        // Clear pending status
        delete pendingTranslations[cacheKey];
        
        return data.translated;
      }
      
      // If we got here, no usable translation
      log(`No usable translation for "${key}", using original text`);
      delete pendingTranslations[cacheKey];
      return defaultText;
    } catch (error) {
      logError(`API error when translating "${key}":`, error);
      delete pendingTranslations[cacheKey];
      return defaultText;
    }
  }
  
  /**
   * Update all elements with a specific translation key
   * @param {string} key - Translation key that was updated
   * @param {string} translation - New translation text
   */
  function updateTranslatedElements(key, translation) {
    log(`Updating all elements with key "${key}" to new translation`);
    
    let updateCount = 0;
    
    // Update text content translations
    document.querySelectorAll(`[data-i18n="${key}"]`).forEach(element => {
      element.textContent = translation;
      updateCount++;
    });
    
    // Update HTML translations
    document.querySelectorAll(`[data-i18n-html="${key}"]`).forEach(element => {
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
          if (attrKey === key) {
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
   * @param {string} key - Translation key
   * @param {object} options - Options including defaultValue
   * @returns {string} - Translated text or default
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
    
    // Create cache key
    const optionsKey = Object.keys(options).length > 0 ? 
      `:${JSON.stringify(options)}` : '';
    
    const cacheKey = `${language}:${namespace}:${translationKey}${optionsKey}`;
    
    // Check cache and return immediately if found
    if (translationsCache[cacheKey]) {
      return translationsCache[cacheKey];
    }
    
    // Not in cache, schedule async fetch for future use
    if (!pendingTranslations[cacheKey]) {
      log(`Scheduling async fetch for "${key}"`);
      
      pendingTranslations[cacheKey] = true;
      
      fetchTranslation(key, defaultValue, options)
        .then(translation => {
          translationsCache[cacheKey] = translation;
          delete pendingTranslations[cacheKey];
          
          // If the translation is different, update DOM elements
          if (translation !== defaultValue) {
            updateTranslatedElements(key, translation);
          }
        })
        .catch(() => {
          delete pendingTranslations[cacheKey];
        });
    }
    
    // Return default for now
    return defaultValue;
  }
  
  // Initialize when DOM is ready
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }
  
  // Export public API
  window.i18n = {
    init,
    t,
    getCurrentLanguage,
    changeLanguage: setLanguage,
    translateDynamicElement,
    updatePageTranslations: translatePageElements,
    syncLanguageMasks,
    isInitialized: () => isInitialized
  };
})();