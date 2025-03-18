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

    // Set initialization flag
    initialized = true;

    // Generate event that i18n is initialized
    document.dispatchEvent(new CustomEvent('i18n:initialized', {
      detail: { language: currentLanguage }
    }));
    
    console.log("i18n successfully initialized");
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

  // ... [rest of the i18n.js implementation remains the same] ...

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
    syncLanguageMasks // Export the sync function for external use
  };

  // Unified setLanguage function exposed to global scope
  window.setLanguage = function(langId) {
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
