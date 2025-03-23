// html/js/i18n.js

/**
 * Client-side multilanguage support module
 */
(function () {
  // Default language (fallback)
  let defaultLanguage = 'en';

  // Current language (initially null, will be set after initialization)
  let currentLanguage = null;

  // Initialization flag
  let initialized = false;

  // Track which namespaces we've loaded
  let loadedNamespaces = {};

  // Check supported languages
  const supportedLanguages = [
    // EU official languages
    'de', 'en', 'fr', 'it', 'es', 'pt', 'nl', 'da', 'sv', 'fi',
    'el', 'cs', 'pl', 'hu', 'sk', 'sl', 'et', 'lv', 'lt', 'ro',
    'bg', 'hr', 'ga', 'mt',
    // Additional languages
    'ru', 'tr', 'ar', 'zh', 'uk', 'sr', 'he', 'ko', 'ja'
  ];

  // Translation cache
  let translationCache = {};

  // Translation retry tracking
  let translationRetryCount = {};
  const MAX_RETRIES = 3;
  const RETRY_DELAYS = [3000, 10000, 30000]; // 3sec, 10sec, 30sec

  // Set of keys currently being translated to avoid duplicate requests
  const pendingTranslations = new Set();

  /**
   * Asynchronous module initialization
   */
  async function init() {
    console.log("Initializing i18n...");

    // Check DOM state
    if (document.readyState !== 'complete' && document.readyState !== 'interactive') {
      console.log("DOM not ready, waiting for load...");
      await new Promise(resolve => {
        if (document.readyState === 'complete' || document.readyState === 'interactive') {
          resolve();
        } else {
          document.addEventListener('DOMContentLoaded', resolve);
        }
      });
    }

    // Determine current language with priorities:
    // 1. Previously stored language in cookie
    // 2. Previously stored language in localStorage
    // 3. Browser language
    // 4. Default language

    let detectedLanguage =
      getCookie('i18next') ||
      localStorage.getItem('i18nextLng') ||
      navigator.language.split('-')[0] ||
      defaultLanguage;

    // If the detected language is not supported, use default
    if (!supportedLanguages.includes(detectedLanguage)) {
      console.log(`Language ${detectedLanguage} is not supported, using ${defaultLanguage}`);
      detectedLanguage = defaultLanguage;
    }

    // Set the current language
    currentLanguage = detectedLanguage;
    console.log(`Set language: ${currentLanguage}`);

    // Set HTML lang attribute
    document.documentElement.lang = currentLanguage;

    // Add dir attribute for RTL languages
    if (['ar', 'he'].includes(currentLanguage)) {
      document.documentElement.dir = 'rtl';
    } else {
      document.documentElement.dir = 'ltr';
    }

    // Save language in cookies and localStorage
    setCookie('i18next', currentLanguage, 365);
    localStorage.setItem('i18nextLng', currentLanguage);

    // Synchronize with window.language for SVG buttons
    window.language = currentLanguage;

    // Synchronize SVG language buttons
    syncLanguageMasks();

    // Setup language switcher
    setupLanguageSwitcher();

    // Update translations only if not the default language
    if (currentLanguage !== defaultLanguage) {
      try {
        // Preload common namespaces
        await preloadCommonNamespaces();

        // Initialize translations on page
        updatePageTranslations();
      } catch (error) {
        console.error('Error during translation initialization:', error);
      }
    }

    // Clean URL from cache busting parameter after page load
    if (window.location.search.includes('i18n_cb')) {
      // Wait for page to fully load
      setTimeout(() => {
        // Use History API to clean URL without page reload
        const cleanUrl = new URL(window.location.href);
        cleanUrl.searchParams.delete('i18n_cb');
        window.history.replaceState({}, document.title, cleanUrl.toString());
        console.log('Removed cache busting parameter from URL');
      }, 2000); // 2 seconds delay to ensure page loaded
    }



    // Set initialization flag
    initialized = true;

    // Generate event that i18n is initialized
    document.dispatchEvent(new CustomEvent('i18n:initialized', {
      detail: { language: currentLanguage }
    }));

    console.log("i18n successfully initialized");
  }

  /**
   * Load translation files with cache busting
   */
  async function loadTranslationFile(language, namespace) {
    const cacheBuster = Date.now();
    const url = `/locales/${language}/${namespace}.json?_=${cacheBuster}`;

    try {
      const response = await fetch(url);

      if (!response.ok) {
        throw new Error(`Failed to load ${namespace} translation for ${language}`);
      }

      return await response.json();
    } catch (error) {
      console.error(`Translation loading error:`, error);
      return null;
    }
  }

  /**
   * Preload common namespaces for current language
   */
  async function preloadCommonNamespaces() {
    if (currentLanguage === defaultLanguage) return;

    const namespaces = ['common', 'auth', 'rma', 'dashboard'];
    const timestamp = Date.now();

    for (const namespace of namespaces) {
      const cacheKey = `${currentLanguage}:${namespace}`;

      // Skip if already loaded
      if (loadedNamespaces[cacheKey]) continue;

      try {
        // Add cache busting parameter
        const response = await fetch(`/locales/${currentLanguage}/${namespace}.json?v=${timestamp}`);

        if (response.ok) {
          const translation = await response.json();
          translationCache[cacheKey] = translation;
          loadedNamespaces[cacheKey] = true;
        }
      } catch (error) {
        console.warn(`Failed to preload ${namespace} for ${currentLanguage}`, error);
      }
    }
  }

  /**
   * Synchronize all language mask elements with current language
   */
  function syncLanguageMasks() {
    try {
      // Close all language masks
      supportedLanguages.forEach(lang => {
        const maskElement = document.getElementById(`${lang}Mask`);
        if (maskElement) {
          maskElement.setAttribute("mask", "url(#maskClose)");
        }
      });

      // Open current language mask
      const currentMask = document.getElementById(`${currentLanguage}Mask`);
      if (currentMask) {
        currentMask.setAttribute("mask", "url(#maskOpen)");
      }
    } catch (e) {
      console.warn("Failed to synchronize SVG language masks:", e);
    }
  }

  /**
   * Change current language
   * @param {string} lang - Language code
   */
  function changeLanguage(lang) {
    if (lang === currentLanguage) return;

    console.log(`i18n.changeLanguage: switching to ${lang}`);

    // Save previous language
    const previousLanguage = currentLanguage;

    // Update current language
    currentLanguage = lang;
    document.documentElement.lang = lang;

    // For RTL languages
    if (['ar', 'he'].includes(lang)) {
      document.documentElement.dir = 'rtl';
    } else {
      document.documentElement.dir = 'ltr';
    }

    // Update cookie and localStorage
    setCookie('i18next', lang, 365);
    localStorage.setItem('i18nextLng', lang);

    // Force reload with cache busting for translations
    if (this._preventReload !== true) {
      const cacheBuster = Date.now();
      const url = new URL(window.location.href);
      url.searchParams.set('i18n_cb', cacheBuster);



      window.location.href = url.toString();
      return;
    }

    // Clear retry counters on language change
    translationRetryCount = {};
    pendingTranslations.clear();
    loadedNamespaces = {};
    translationCache = {};

    // Update global window.language for SVG sync
    window.language = lang;

    // Synchronize SVG buttons
    syncLanguageMasks();

    // Check translation files for new language
    checkTranslationFiles();

    // Only update translations if not default language
    if (lang !== defaultLanguage) {
      // Preload common namespaces for new language
      preloadCommonNamespaces().then(() => {
        // Update translations on page
        updatePageTranslations();
      });
    } else {
      // If switching to default language
      if (this._reloadOnDefault !== false) {
        // By default reload page
        window.location.reload();
        return;
      } else {
        // Force update content without reload if flag set
        updatePageTranslations();
        console.log("Switching to default language without page reload");
      }
    }

    // Update active class on language options
    const options = document.querySelectorAll('.language-option, .language-item');
    options.forEach(option => {
      if (option.dataset.lang === lang) {
        option.classList.add('active');
      } else {
        option.classList.remove('active');
      }
    });

    // Generate language change event
    document.dispatchEvent(new CustomEvent('languageChanged', {
      detail: { language: lang }
    }));
  }

  /**
   * Check if translation files exist for the current language
   */
  async function checkTranslationFiles() {
    if (currentLanguage === defaultLanguage) return;

    const namespaces = ['common', 'auth', 'rma', 'dashboard'];
    const timestamp = Date.now();

    console.log(`Checking translation files for ${currentLanguage}...`);

    for (const namespace of namespaces) {
      try {
        const response = await fetch(`/locales/${currentLanguage}/${namespace}.json?v=${timestamp}`, {
          method: 'HEAD'
        });

        if (!response.ok) {
          console.warn(`Missing translation file: ${currentLanguage}/${namespace}.json`);
        }
      } catch (error) {
        console.warn(`Error checking translation file: ${currentLanguage}/${namespace}.json`, error);
      }
    }
  }

  /**
   * Get translation for a key
   * @param {string} key - Translation key, can include namespace prefix (e.g. 'common:welcome')
   * @param {Object} options - Options for interpolation
   * @returns {string} - Translated text or key if not found
   */
  function getTranslation(key, options = {}) {
    // Skip translation if we're using default language
    if (currentLanguage === defaultLanguage) {
      return interpolate(key, options);
    }

    // Parse namespace from key
    const parts = key.split(':');
    const namespace = parts.length > 1 ? parts[0] : 'common';
    const actualKey = parts.length > 1 ? parts.slice(1).join(':') : key;

    // Build cache key
    const cacheKey = `${currentLanguage}:${namespace}:${actualKey}`;

    // Check if translation exists in cache
    if (translationCache[cacheKey]) {
      return interpolate(translationCache[cacheKey], options);
    }

    // If namespace is not loaded, queue it for loading
    const namespaceKey = `${currentLanguage}:${namespace}`;
    if (!loadedNamespaces[namespaceKey] && !pendingTranslations.has(cacheKey)) {
      pendingTranslations.add(cacheKey);

      // Schedule background load with a slight delay
      setTimeout(() => {
        loadNamespace(currentLanguage, namespace).then(() => {
          pendingTranslations.delete(cacheKey);
          // Trigger update for elements with this key
          updateTranslationsForKey(key);
        }).catch(error => {
          console.error(`Failed to load namespace ${namespace} for key ${key}:`, error);
          pendingTranslations.delete(cacheKey);
        });
      }, 10);
    }

    // Return original key as fallback
    return interpolate(key, options);
  }

  /**
   * Interpolate variables in translation string
   * @param {string} text - Text with variables
   * @param {Object} options - Variables for interpolation
   * @returns {string} - Interpolated text
   */
  function interpolate(text, options) {
    if (!options || typeof text !== 'string') return text;

    return text.replace(/{{([\s\S]*?)}}/g, (match, key) => {
      const value = key.trim().split('.').reduce((obj, k) => obj?.[k], options);
      return value !== undefined ? value : match;
    });
  }

  /**
   * Load a namespace for a language
   * @param {string} language - Language code
   * @param {string} namespace - Namespace to load
   * @returns {Promise} - Promise that resolves when namespace is loaded
   */
  async function loadNamespace(language, namespace) {
    const namespaceKey = `${language}:${namespace}`;

    // Skip if already loaded
    if (loadedNamespaces[namespaceKey]) {
      return Promise.resolve();
    }

    try {
      const translations = await loadTranslationFile(language, namespace);

      if (translations) {
        // Add translations to cache
        for (const [key, value] of Object.entries(translations)) {
          translationCache[`${language}:${namespace}:${key}`] = value;
        }

        // Mark namespace as loaded
        loadedNamespaces[namespaceKey] = true;
      }
    } catch (error) {
      console.error(`Failed to load namespace ${namespace} for ${language}:`, error);
    }
  }

  /**
   * Update translations for elements with specific key
   * @param {string} key - Translation key to update
   */
  function updateTranslationsForKey(key) {
    // Find elements with this key
    document.querySelectorAll(`[data-i18n="${key}"]`).forEach(el => {
      const translation = getTranslation(key);
      if (translation !== key) {
        el.textContent = translation;
      }
    });
  }

  /**
   * Update translations on the current page
   */
  function updatePageTranslations() {
    // Skip if using default language
    if (currentLanguage === defaultLanguage) return;

    // Process all elements with data-i18n attribute
    document.querySelectorAll('[data-i18n]').forEach(el => {
      const key = el.getAttribute('data-i18n');
      const translation = getTranslation(key);

      if (translation !== key) {
        el.textContent = translation;
      }
    });

    // Process attribute translations with data-i18n-attr
    document.querySelectorAll('[data-i18n-attr]').forEach(el => {
      try {
        const attrsMap = JSON.parse(el.getAttribute('data-i18n-attr'));

        for (const [attr, key] of Object.entries(attrsMap)) {
          const translation = getTranslation(key);

          if (translation !== key) {
            el.setAttribute(attr, translation);
          }
        }
      } catch (e) {
        console.error('Error parsing data-i18n-attr:', e);
      }
    });

    // Process HTML translations with data-i18n-html
    document.querySelectorAll('[data-i18n-html]').forEach(el => {
      const key = el.getAttribute('data-i18n-html');
      const translation = getTranslation(key);

      if (translation !== key) {
        el.innerHTML = translation;
      }
    });
  }

  /**
   * Create a simple language switcher UI
   */
  function setupLanguageSwitcher() {
    // Create a simple language switcher that avoids page reload
    const languages = {
      'en': 'English',
      'de': 'Deutsch',
      'ko': '한국어',
      'fr': 'Français',
      'cs': 'Čeština',
      'pl': 'Polski'
    };

    // Find a good place to insert the language switcher
    const container = document.createElement('div');
    container.className = 'i18n-language-switcher';
    container.style.position = 'fixed';
    container.style.bottom = '20px';
    container.style.left = '20px';
    container.style.zIndex = '1000';
    container.style.backgroundColor = 'rgba(255,255,255,0.8)';
    container.style.padding = '5px';
    container.style.borderRadius = '5px';

    Object.entries(languages).forEach(([code, name]) => {
      const button = document.createElement('button');
      button.textContent = name;
      button.onclick = () => window.i18n.changeLanguage(code);
      button.style.margin = '2px';
      button.style.padding = '3px 8px';
      button.style.backgroundColor = currentLanguage === code ? '#1e2071' : '#f0f0f0';
      button.style.color = currentLanguage === code ? 'white' : 'black';
      button.style.border = 'none';
      button.style.borderRadius = '3px';
      button.style.cursor = 'pointer';
      container.appendChild(button);
    });

    document.body.appendChild(container);
  }

  // Helper function to set cookie
  function setCookie(name, value, days) {
    let expires = '';
    if (days) {
      const date = new Date();
      date.setTime(date.getTime() + (days * 24 * 60 * 60 * 1000));
      expires = '; expires=' + date.toUTCString();
    }
    document.cookie = name + '=' + encodeURIComponent(value) + expires + '; path=/';
  }

  // Helper function to get cookie
  function getCookie(name) {
    const nameEQ = name + '=';
    const ca = document.cookie.split(';');
    for (let i = 0; i < ca.length; i++) {
      let c = ca[i];
      while (c.charAt(0) === ' ') c = c.substring(1, c.length);
      if (c.indexOf(nameEQ) === 0) {
        return decodeURIComponent(c.substring(nameEQ.length, c.length));
      }
    }
    return null;
  }

  // Export to window.i18n
  window.i18n = {
    debugMode: false,
    toggleDebug: function () {
      this.debugMode = !this.debugMode;
      console.log(`i18n debug mode ${this.debugMode ? 'enabled' : 'disabled'}`);
      if (this.debugMode) {
        this.checkTranslationStructure();
      }
    },
    checkTranslationStructure: function () {
      console.log('===== LOADED TRANSLATIONS =====');
      console.log('Translation cache:', translationCache);
      console.log('Loaded namespaces:', loadedNamespaces);
      console.log('Current language:', currentLanguage);
    },
    init,
    changeLanguage,
    getCurrentLanguage: () => currentLanguage || defaultLanguage,
    updatePageTranslations,
    translateDynamicElement: (element, context = '') => {
      if (!element) return Promise.resolve();

      // Skip if default language or not initialized
      if (window.i18n.getCurrentLanguage() === 'en' || !window.i18n.isInitialized()) {
        return Promise.resolve();
      }

      // Process all elements with translation attributes
      const processElement = (el) => {
        // Handle data-i18n attribute
        if (el.hasAttribute('data-i18n')) {
          const key = el.getAttribute('data-i18n');
          const translation = window.i18n.t(key);

          if (translation !== key) {
            el.textContent = translation;
          }
        }

        // Handle data-i18n-attr attribute
        if (el.hasAttribute('data-i18n-attr')) {
          try {
            const attrsMap = JSON.parse(el.getAttribute('data-i18n-attr'));
            for (const [attr, key] of Object.entries(attrsMap)) {
              const translation = window.i18n.t(key);
              if (translation !== key) {
                el.setAttribute(attr, translation);
              }
            }
          } catch (e) {
            console.error('Error parsing data-i18n-attr:', e);
          }
        }

        // Handle data-i18n-html attribute
        if (el.hasAttribute('data-i18n-html')) {
          const key = el.getAttribute('data-i18n-html');
          const translation = window.i18n.t(key, { interpolation: { escapeValue: false } });

          if (translation !== key) {
            el.innerHTML = translation;
          }
        }

        // Process child elements
        Array.from(el.children).forEach(processElement);
      };

      // Process the element
      processElement(element);

      return Promise.resolve();
    },
    isInitialized: () => initialized,
    t: getTranslation,
    checkTranslationFiles,
    exists: function (key) {
      const parts = key.split(':');
      const namespace = parts.length > 1 ? parts[0] : 'common';
      const actualKey = parts.length > 1 ? parts.slice(1).join(':') : key;
      const cacheKey = `${currentLanguage}:${namespace}:${actualKey}`;
      return translationCache[cacheKey] !== undefined;
    },
    syncLanguageMasks, // Export the sync function for external use
    loadTranslationFile // Export for external use
  };

  // Unified setLanguage function exposed to global scope
  window.setLanguage = function (langId) {
    // Map of language codes
    const langMap = {
      'lang1': 'en',
      'lang2': 'de',
      'lang3': 'pl',
      'lang4': 'cs',
      'lang5': 'fr',
      'lang6': 'ko'
    };

    let lang;

    // If langId starts with 'lang', get language from map
    if (typeof langId === 'string' && langId.startsWith('lang')) {
      lang = langMap[langId];
      if (!lang) {
        console.error(`Unknown language ID: ${langId}`);
        return;
      }
    } else {
      // Otherwise, treat as direct language code
      lang = langId;
    }

    // Check if language is supported
    if (!supportedLanguages.includes(lang)) {
      console.error(`Language ${lang} is not supported`);
      return;
    }

    // Use i18n.changeLanguage to set language
    if (window.i18n && window.i18n.changeLanguage) {
      window.i18n.changeLanguage(lang);
    } else {
      console.error('i18n not initialized');

      // Fallback if i18n not initialized - update cookies and reload
      setCookie('i18next', lang, 365);
      try {
        localStorage.setItem('i18nextLng', lang);
      } catch (e) {
        console.warn("Failed to save language to localStorage", e);
      }
      window.location.reload();
    }
  };

  // Call async initialization when script loads
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', () => {
      // Check if initialization already happened
      if (!window.i18nInitStarted) {
        window.i18nInitStarted = true;
        console.log("i18n self-initializing on DOMContentLoaded");
        init();
      }
    });
  } else {
    // Check if initialization already happened
    if (!window.i18nInitStarted) {
      window.i18nInitStarted = true;
      console.log("i18n self-initializing immediately");
      init();
    }
  }
})();