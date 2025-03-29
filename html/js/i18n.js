// html/js/i18n.js - Updated implementation with cache busting
(function() {
  // Default language fallback
  const defaultLanguage = 'en';
  
  // Store translations cache
  const translationsCache = {};
  
  // Track initialization state
  let isInitialized = false;
  
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
    
    if (newLanguage === previousLanguage) return;
    
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
    window.location.href = url.toString();
  }
  
  /**
   * Initialize i18n with meta tag prioritization
   */
  function init() {
    if (isInitialized) return Promise.resolve();
    
    // Get language from meta tag (server-provided)
    const language = getCurrentLanguage();
    
    console.log(`[i18n] Initializing with language: ${language}`);
    
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
      translatePageElements();
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
        console.log('[i18n] Removed cache busting parameter from URL');
      }, 2000); // 2 seconds delay to ensure page loaded
    }
    
    // Mark as initialized
    isInitialized = true;
    
    // Fire initialized event for other scripts
    document.dispatchEvent(new CustomEvent('i18n:initialized', {
      detail: { language }
    }));
    
    return Promise.resolve(language);
  }
  
  /**
   * Setup fetch interception to add language headers to all requests
   */
  function setupFetchInterception() {
    const originalFetch = window.fetch;
    window.fetch = function(url, options = {}) {
      if (!options.headers) {
        options.headers = {};
      }
      
      // Add app-language header to all fetch requests
      if (typeof options.headers.set === 'function') {
        options.headers.set('app-language', getCurrentLanguage());
      } else {
        options.headers['app-language'] = getCurrentLanguage();
      }
      
      return originalFetch.call(this, url, options);
    };
  }
  
  /**
   * Setup mutation observer to translate dynamically added elements
   */
  function setupMutationObserver() {
    if (!window.MutationObserver) return;
    
    const observer = new MutationObserver(mutations => {
      for (const mutation of mutations) {
        if (mutation.type === 'childList' && mutation.addedNodes.length) {
          for (const node of mutation.addedNodes) {
            if (node.nodeType === Node.ELEMENT_NODE) {
              translateDynamicElement(node);
            }
          }
        }
      }
    });
    
    // Start observing
    observer.observe(document.body, {
      childList: true,
      subtree: true
    });
  }
  
  /**
   * Translate an element and its children
   * @param {HTMLElement} element - Element to translate
   */
  function translateDynamicElement(element) {
    if (getCurrentLanguage() === defaultLanguage) return;
    
    // Process standard translations
    element.querySelectorAll('[data-i18n]').forEach(processElementTranslation);
    if (element.hasAttribute('data-i18n')) {
      processElementTranslation(element);
    }
    
    // Process attribute translations
    element.querySelectorAll('[data-i18n-attr]').forEach(processAttrTranslation);
    if (element.hasAttribute('data-i18n-attr')) {
      processAttrTranslation(element);
    }
    
    // Process HTML translations
    element.querySelectorAll('[data-i18n-html]').forEach(processHtmlTranslation);
    if (element.hasAttribute('data-i18n-html')) {
      processHtmlTranslation(element);
    }
  }
  
  /**
   * Process element text content translation
   * @param {HTMLElement} element - Element to translate
   */
  function processElementTranslation(element) {
    const key = element.getAttribute('data-i18n');
    if (!key) return;
    
    const originalText = element.textContent.trim();
    
    // Use API translation for this key/text
    fetchTranslation(key, originalText).then(translation => {
      if (translation && translation !== originalText) {
        element.textContent = translation;
      }
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
      
      for (const [attr, key] of Object.entries(attrs)) {
        const originalValue = element.getAttribute(attr) || '';
        
        // Use API translation for this key/value
        fetchTranslation(key, originalValue).then(translation => {
          if (translation && translation !== originalValue) {
            element.setAttribute(attr, translation);
          }
        });
      }
    } catch (error) {
      console.error('Error processing attribute translation:', error);
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
    
    // Use API translation for this key/html
    fetchTranslation(key, originalHtml).then(translation => {
      if (translation && translation !== originalHtml) {
        element.innerHTML = translation;
      }
    });
  }
  
  /**
   * Translate all page elements
   */
  function translatePageElements() {
    // Process standard translations
    document.querySelectorAll('[data-i18n]').forEach(processElementTranslation);
    
    // Process attribute translations
    document.querySelectorAll('[data-i18n-attr]').forEach(processAttrTranslation);
    
    // Process HTML translations
    document.querySelectorAll('[data-i18n-html]').forEach(processHtmlTranslation);
  }
  
  /**
   * Fetch translation from server or cache with cache busting
   * @param {string} key - Translation key
   * @param {string} defaultText - Default text if no translation found
   * @returns {Promise<string>} - Translation or default text
   */
  async function fetchTranslation(key, defaultText) {
    const language = getCurrentLanguage();
    
    // Skip translation for default language
    if (language === defaultLanguage) {
      return Promise.resolve(defaultText);
    }
    
    const cacheKey = `${language}:${key}`;
    
    // Check cache first
    if (translationsCache[cacheKey]) {
      return Promise.resolve(translationsCache[cacheKey]);
    }
    
    try {
      // Extract namespace from key (format: namespace:key)
      let namespace = 'common';
      let translationKey = key;
      
      if (key.includes(':')) {
        const parts = key.split(':');
        namespace = parts[0];
        translationKey = parts.slice(1).join(':');
      }
      
      // Add cache busting to API call
      const cacheBuster = Date.now();
      
      // Call translation API
      const response = await fetch(`/api/translate?_=${cacheBuster}`, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          'app-language': language
        },
        body: JSON.stringify({
          text: defaultText,
          targetLang: language,
          context: namespace,
          key: translationKey
        })
      });
      
      if (!response.ok) {
        return defaultText;
      }
      
      const data = await response.json();
      
      // Cache the result
      if (data.translated && data.translated !== defaultText) {
        translationsCache[cacheKey] = data.translated;
        return data.translated;
      }
      
      return defaultText;
    } catch (error) {
      console.error('Translation error:', error);
      return defaultText;
    }
  }
  
  /**
   * Update SVG language masks to show active language
   */
  function syncLanguageMasks() {
    try {
      const currentLang = getCurrentLanguage();
      
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
      console.warn("Error synchronizing language masks:", e);
    }
  }
  
  /**
   * Load namespace with cache busting
   * @param {string} language - Language code
   * @param {string} namespace - Namespace to load
   * @returns {Promise<object>} - Translation data
   */
  async function loadNamespace(language, namespace) {
    // Add cache busting timestamp
    const timestamp = Date.now();
    const url = `/locales/${language}/${namespace}.json?v=${timestamp}`;
    
    try {
      const response = await fetch(url);
      
      if (!response.ok) {
        throw new Error(`Failed to load namespace ${namespace} for ${language}`);
      }
      
      return await response.json();
    } catch (error) {
      console.error(`Translation namespace loading error:`, error);
      return null;
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
    
    const cacheKey = `${language}:${key}`;
    
    // Check cache and return immediately if found
    if (translationsCache[cacheKey]) {
      return translationsCache[cacheKey];
    }
    
    // Schedule async fetch for future use
    fetchTranslation(key, defaultValue).then(translation => {
      translationsCache[cacheKey] = translation;
    });
    
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
    isInitialized: () => isInitialized,
    loadNamespace
  };
})();