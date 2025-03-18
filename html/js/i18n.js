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
    // Determine current language
    currentLanguage =
      getCookie('i18next') ||
      localStorage.getItem('i18nextLng') ||
      navigator.language.split('-')[0] ||
      defaultLanguage;

    // If current language not supported, use default
    if (!supportedLanguages.includes(currentLanguage)) {
      currentLanguage = defaultLanguage;
    }

    // Set lang attribute for HTML
    document.documentElement.lang = currentLanguage;

    // Add dir attribute for RTL languages
    if (['ar', 'he'].includes(currentLanguage)) {
      document.documentElement.dir = 'rtl';
    } else {
      document.documentElement.dir = 'ltr';
    }

    // Save language in cookie and localStorage
    setCookie('i18next', currentLanguage, 365); // for 365 days
    localStorage.setItem('i18nextLng', currentLanguage);

    // Initialize language switcher first
    setupLanguageSwitcher();

    // Only update translations if language is not default
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

    // Dispatch event that i18n is initialized
    document.dispatchEvent(new CustomEvent('i18n:initialized', {
      detail: { language: currentLanguage }
    }));
  }

  /**
   * This script adds the missing translateDynamicElement function to the i18n.js implementation
   * Load this script after i18n.js to fix the error
   */
  (function () {
    // Wait for i18n to be initialized
    function checkAndExtendI18n() {
      if (window.i18n) {
        // Only add the function if it doesn't exist or is causing errors
        if (!window.i18n.translateDynamicElement || window.i18n.translateDynamicElement.toString().includes('return translateDynamicElement')) {

          // Define the actual translation function
          window.i18n.translateDynamicElement = function (element, context = '') {
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
                console.log('data-i18n key', key);
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
                    console.log('data-i18n-attr key', key);
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
                console.log('data-i18n-html key', key);
                const translation = window.i18n.t(key, { interpolation: { escapeValue: false } });

                if (translation !== key) {
                  el.innerHTML = translation;
                }
              }
            };

            // Process the element itself
            processElement(element);

            // Process all child elements with translation attributes
            element.querySelectorAll('[data-i18n], [data-i18n-attr], [data-i18n-html]').forEach(el => {
              processElement(el);
            });

            return Promise.resolve();
          };

          console.log('Fixed translateDynamicElement function added to i18n');
        }
      } else {
        // If i18n isn't loaded yet, check again in 100ms
        setTimeout(checkAndExtendI18n, 100);
      }
    }

    // Start checking for i18n
    checkAndExtendI18n();
  })();


  /**
   * Preload common namespaces
   */
  async function preloadCommonNamespaces() {
    const commonNamespaces = ['common', 'auth', 'rma', 'dashboard'];

    console.log(`Preloading namespaces for language: ${currentLanguage}`);

    // Create array of promises for parallel loading
    const loadPromises = commonNamespaces.map(async (namespace) => {
      // Skip if already loaded
      if (loadedNamespaces[`${currentLanguage}:${namespace}`]) {
        console.log(`Namespace ${namespace} already loaded, skipping`);
        return;
      }

      const localePath = `/locales/${currentLanguage}/${namespace}.json`;
      try {
        console.log(`Attempting to load namespace: ${namespace} from ${localePath}`);
        const response = await fetch(localePath);

        if (response.ok) {
          const translations = await response.json();
          // Cache translations
          for (const [k, v] of Object.entries(translations)) {
            const fullKey = `${currentLanguage}:${namespace}:${k}`;
            translationCache[fullKey] = v;
            console.log(`Cached translation for key: ${fullKey}`);
          }
          loadedNamespaces[`${currentLanguage}:${namespace}`] = true;
          console.log(`Successfully loaded namespace: ${namespace} with ${Object.keys(translations).length} keys`);
        } else {
          console.warn(`Failed to load namespace ${namespace}: HTTP ${response.status}`);
        }
      } catch (error) {
        console.error(`Error loading namespace ${namespace}: ${error.message}`);
      }
    });

    // Wait for all namespaces to load
    await Promise.all(loadPromises);

    // Log cache status
    console.log('Translation cache status:', {
      loadedNamespaces,
      cacheSize: Object.keys(translationCache).length
    });
  }

  /**
   * Check if translation files exist
   */
  function checkTranslationFiles() {
    if (currentLanguage === defaultLanguage) return;

    const commonNamespaces = ['common', 'auth', 'rma', 'dashboard'];

    console.log(`----- CHECKING TRANSLATION FILES FOR ${currentLanguage} -----`);

    for (const namespace of commonNamespaces) {
      const localePath = `/locales/${currentLanguage}/${namespace}.json`;
      fetch(localePath)
        .then(response => {
          console.log(`${localePath}: ${response.ok ? 'OK ✓' : 'NOT FOUND ✗'}`);
          if (!response.ok) {
            console.warn(`HTTP Status: ${response.status}`);
          }
        })
        .catch(error => {
          console.error(`Error checking ${localePath}: ${error.message}`);
        });
    }
  }

  /**
   * Setup language switcher
   */
  function setupLanguageSwitcher() {
    // Find all language selectors
    const languageSelectors = document.querySelectorAll('.language-selector');
    if (languageSelectors.length === 0) return;

    // Flags and names for main languages
    const languages = [
      { code: 'de', name: 'Deutsch', flag: '🇩🇪' },
      { code: 'en', name: 'English', flag: '🇬🇧' },
      { code: 'fr', name: 'Français', flag: '🇫🇷' },
      { code: 'it', name: 'Italiano', flag: '🇮🇹' },
      { code: 'es', name: 'Español', flag: '🇪🇸' },
      { code: 'ru', name: 'Русский', flag: '🇷🇺' },
      { code: 'zh', name: '中文', flag: '🇨🇳' },
      { code: 'ar', name: 'العربية', flag: '🇸🇦' },
      { code: 'ja', name: '日本語', flag: '🇯🇵' }
    ];

    // Update each language selector
    languageSelectors.forEach(languageSelector => {
      // Clear content
      languageSelector.innerHTML = '';

      // Create selector options
      languages.forEach(lang => {
        const option = document.createElement('div');
        option.className = 'language-option';
        option.dataset.lang = lang.code;
        option.innerHTML = `${lang.flag} <span>${lang.name}</span>`;

        if (lang.code === currentLanguage) {
          option.classList.add('active');
        }

        option.addEventListener('click', function () {
          changeLanguage(lang.code);
        });

        languageSelector.appendChild(option);
      });

      // "More languages" button
      const moreBtn = document.createElement('div');
      moreBtn.className = 'language-more-btn';
      moreBtn.textContent = '...';
      moreBtn.addEventListener('click', function () {
        showAllLanguages(languageSelector);
      });

      languageSelector.appendChild(moreBtn);
    });
  }

  /**
   * Show modal with all available languages
   */
  function showAllLanguages(container) {
    // Full language list
    const allLanguages = [
      // EU
      { code: 'de', name: 'Deutsch', flag: '🇩🇪' },
      { code: 'en', name: 'English', flag: '🇬🇧' },
      { code: 'fr', name: 'Français', flag: '🇫🇷' },
      { code: 'it', name: 'Italiano', flag: '🇮🇹' },
      { code: 'es', name: 'Español', flag: '🇪🇸' },
      { code: 'pt', name: 'Português', flag: '🇵🇹' },
      { code: 'nl', name: 'Nederlands', flag: '🇳🇱' },
      { code: 'da', name: 'Dansk', flag: '🇩🇰' },
      { code: 'sv', name: 'Svenska', flag: '🇸🇪' },
      { code: 'fi', name: 'Suomi', flag: '🇫🇮' },
      { code: 'el', name: 'Ελληνικά', flag: '🇬🇷' },
      { code: 'cs', name: 'Čeština', flag: '🇨🇿' },
      { code: 'pl', name: 'Polski', flag: '🇵🇱' },
      { code: 'hu', name: 'Magyar', flag: '🇭🇺' },
      { code: 'sk', name: 'Slovenčina', flag: '🇸🇰' },
      { code: 'sl', name: 'Slovenščina', flag: '🇸🇮' },
      { code: 'et', name: 'Eesti', flag: '🇪🇪' },
      { code: 'lv', name: 'Latviešu', flag: '🇱🇻' },
      { code: 'lt', name: 'Lietuvių', flag: '🇱🇹' },
      { code: 'ro', name: 'Română', flag: '🇷🇴' },
      { code: 'bg', name: 'Български', flag: '🇧🇬' },
      { code: 'hr', name: 'Hrvatski', flag: '🇭🇷' },
      { code: 'ga', name: 'Gaeilge', flag: '🇮🇪' },
      { code: 'mt', name: 'Malti', flag: '🇲🇹' },
      // Additional languages
      { code: 'ru', name: 'Русский', flag: '🇷🇺' },
      { code: 'tr', name: 'Türkçe', flag: '🇹🇷' },
      { code: 'ar', name: 'العربية', flag: '🇸🇦' },
      { code: 'zh', name: '中文', flag: '🇨🇳' },
      { code: 'uk', name: 'Українська', flag: '🇺🇦' },
      { code: 'sr', name: 'Српски', flag: '🇷🇸' },
      { code: 'he', name: 'עברית', flag: '🇮🇱' },
      { code: 'ko', name: '한국어', flag: '🇰🇷' },
      { code: 'ja', name: '日本語', flag: '🇯🇵' }
    ];

    // Create modal
    const modal = document.createElement('div');
    modal.className = 'language-modal';
    modal.innerHTML = `
      <div class="language-modal-content">
        <div class="language-modal-header">
          <h3>Sprache auswählen / Select Language</h3>
          <button class="language-modal-close">&times;</button>
        </div>
        <div class="language-modal-body">
          <div class="language-grid"></div>
        </div>
      </div>
    `;

    // Add styles
    const style = document.createElement('style');
    style.textContent = `
      .language-modal {
        position: fixed;
        top: 0;
        left: 0;
        width: 100%;
        height: 100%;
        background-color: rgba(0, 0, 0, 0.5);
        display: flex;
        justify-content: center;
        align-items: center;
        z-index: 1000;
      }
      .language-modal-content {
        background-color: white;
        padding: 20px;
        border-radius: 8px;
        max-width: 80%;
        max-height: 80%;
        overflow-y: auto;
      }
      .language-modal-header {
        display: flex;
        justify-content: space-between;
        align-items: center;
        margin-bottom: 20px;
      }
      .language-modal-close {
        background: none;
        border: none;
        font-size: 24px;
        cursor: pointer;
      }
      .language-grid {
        display: grid;
        grid-template-columns: repeat(auto-fill, minmax(150px, 1fr));
        gap: 10px;
      }
      .language-item {
        padding: 10px;
        border: 1px solid #ddd;
        border-radius: 4px;
        cursor: pointer;
        transition: background-color 0.3s;
      }
      .language-item:hover {
        background-color: #f5f5f5;
      }
      .language-item.active {
        background-color: #e6f7ff;
        border-color: #1890ff;
      }
    `;

    document.head.appendChild(style);
    document.body.appendChild(modal);

    // Fill language grid
    const grid = modal.querySelector('.language-grid');

    allLanguages.forEach(lang => {
      const item = document.createElement('div');
      item.className = 'language-item';
      if (lang.code === currentLanguage) {
        item.classList.add('active');
      }
      item.innerHTML = `${lang.flag} ${lang.name}`;
      item.addEventListener('click', function () {
        changeLanguage(lang.code);
        document.body.removeChild(modal);
      });

      grid.appendChild(item);
    });

    // Close handler
    modal.querySelector('.language-modal-close').addEventListener('click', function () {
      document.body.removeChild(modal);
    });

    // Close on click outside modal
    modal.addEventListener('click', function (e) {
      if (e.target === modal) {
        document.body.removeChild(modal);
      }
    });
  }

  /**
   * Change current language
   * @param {string} lang - Language code
   */
  function changeLanguage(lang) {
    if (lang === currentLanguage) return;

    // Save new language
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
      // If switching to default language, reload page to get original content
      window.location.reload();
      return;
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
   * Update translations on page
   */
  function updatePageTranslations() {
    // If language equals default language, no translation needed
    if (currentLanguage === defaultLanguage) return;

    console.log('Starting page translation update');

    // Reset retry counters on full page update
    translationRetryCount = {};
    pendingTranslations.clear();

    // First collect all untranslated elements (with data-i18n attributes)
    const elements = document.querySelectorAll('[data-i18n]');
    console.log(`Found ${elements.length} elements with data-i18n attribute`);

    // Process the elements in batches for better performance
    if (elements.length > 0) {
      processElementsInBatches(elements, 0);
    }

    // Also handle attribute translations
    const attributeElements = document.querySelectorAll('[data-i18n-attr]');
    console.log(`Found ${attributeElements.length} elements with data-i18n-attr attribute`);
    if (attributeElements.length > 0) {
      processAttributeTranslations(attributeElements);
    }

    // Handle HTML content translations
    const htmlElements = document.querySelectorAll('[data-i18n-html]');
    console.log(`Found ${htmlElements.length} elements with data-i18n-html attribute`);
    if (htmlElements.length > 0) {
      processHtmlTranslations(htmlElements);
    }

    console.log('Finished page translation update');
  }

  /**
   * Process elements in batches to avoid UI blocking
   * @param {NodeList} elements - Elements with data-i18n attribute
   * @param {number} startIndex - Start index for current batch
   * @param {number} batchSize - Size of each batch
   */
  function processElementsInBatches(elements, startIndex, batchSize = 50) {
    const endIndex = Math.min(startIndex + batchSize, elements.length);
    const batch = Array.from(elements).slice(startIndex, endIndex);

    // Collect texts to translate
    const textsToTranslate = [];
    const keysMap = [];
    const elementsMap = [];

    batch.forEach(el => {
      const key = el.getAttribute('data-i18n');
      // Get translation for this key
      const translation = getTranslation(key);

      // Apply translation if it's not the same as the key
      if (translation !== key) {
        el.textContent = translation;
      } else {
        // If we didn't find a translation, add to list for processing
        textsToTranslate.push(key);
        keysMap.push(key);
        elementsMap.push(el);
      }
    });

    // If there are more elements, schedule the next batch
    if (endIndex < elements.length) {
      setTimeout(() => {
        processElementsInBatches(elements, endIndex, batchSize);
      }, 0);
    }
  }

  /**
   * Process HTML content translations
   * @param {NodeList} elements - Elements with data-i18n-html attribute
   */
  function processHtmlTranslations(elements) {
    const textsToTranslate = [];
    const keysMap = [];
    const elementsMap = [];

    elements.forEach(el => {
      const key = el.getAttribute('data-i18n-html');

      // Try getting translation with the improved method
      const translation = getTranslation(key);

      // If translation found and it's not the same as the key
      if (translation !== key) {
        el.innerHTML = translation;
      } else {
        textsToTranslate.push(key);
        keysMap.push(key);
        elementsMap.push(el);
      }
    });

    // Only make API calls if needed
    if (textsToTranslate.length > 0) {
      // First check if we can load these from namespaces
      loadNamespacesForKeys(textsToTranslate).then(() => {
        // After loading namespaces, try getting translations again
        elementsMap.forEach((el, index) => {
          const key = keysMap[index];
          const translation = getTranslation(key);

          if (translation !== key) {
            el.innerHTML = translation;
          } else if (!pendingTranslations.has(`${currentLanguage}:html:${key}`)) {
            // If still no translation, request via API
            requestApiTranslation(key, getNamespaceFromKey(key), `${currentLanguage}:html:${key}`, true);
            el.classList.add('i18n-loading');
          }
        });
      });
    }
  }

  /**
   * Process attribute translations
   * @param {NodeList} elements - Elements with data-i18n-attr attribute
   */
  function processAttributeTranslations(elements) {
    const attributeTexts = [];
    const attributeMappings = [];

    elements.forEach(el => {
      try {
        const attrsMap = JSON.parse(el.getAttribute('data-i18n-attr'));
        for (const [attr, key] of Object.entries(attrsMap)) {
          // Try getting translation with the improved method
          const translation = getTranslation(key);

          // If translation found and it's not the same as the key
          if (translation !== key) {
            el.setAttribute(attr, translation);
          } else {
            attributeTexts.push(key);
            attributeMappings.push({
              element: el,
              attribute: attr,
              key: key
            });
          }
        }
      } catch (e) {
        console.error('Error parsing data-i18n-attr:', e);
      }
    });

    // Only make API calls if needed
    if (attributeTexts.length > 0) {
      // First try to load namespaces for these keys
      loadNamespacesForKeys(attributeTexts).then(() => {
        // After loading namespaces, try getting translations again
        attributeMappings.forEach(mapping => {
          const translation = getTranslation(mapping.key);

          if (translation !== mapping.key) {
            mapping.element.setAttribute(mapping.attribute, translation);
          } else if (!pendingTranslations.has(`${currentLanguage}:attr:${mapping.key}`)) {
            // If still no translation, request via API
            requestApiTranslation(
              mapping.key,
              getNamespaceFromKey(mapping.key),
              `${currentLanguage}:attr:${mapping.key}`
            );
            mapping.element.classList.add('i18n-loading');
          }
        });
      });
    }
  }

  /**
   * Load JSON translation files for namespaces needed by keys
   * @param {string[]} keys - Translation keys
   * @returns {Promise} - Promise resolving when files are loaded
   */
  async function loadNamespacesForKeys(keys) {
    // Extract namespaces from keys
    const namespaces = new Set();

    keys.forEach(key => {
      const namespace = getNamespaceFromKey(key);
      if (namespace && !loadedNamespaces[`${currentLanguage}:${namespace}`]) {
        namespaces.add(namespace);
      }
    });

    // Load each namespace
    const promises = [];

    namespaces.forEach(namespace => {
      if (!loadedNamespaces[`${currentLanguage}:${namespace}`]) {
        const promise = loadNamespace(namespace);
        promises.push(promise);
      }
    });

    return Promise.all(promises);
  }


  // Добавить функцию для преобразования вложенных объектов в плоскую структуру
  function flattenObject(obj, prefix = '') {
    return Object.keys(obj).reduce((acc, k) => {
      const pre = prefix.length ? `${prefix}.` : '';
      if (typeof obj[k] === 'object' && obj[k] !== null && !Array.isArray(obj[k])) {
        Object.assign(acc, flattenObject(obj[k], `${pre}${k}`));
      } else {
        acc[`${pre}${k}`] = obj[k];
      }
      return acc;
    }, {});
  }

  /**
   * Load a single namespace translation file
   * @param {string} namespace - Namespace to load
   * @returns {Promise} - Promise resolving when file is loaded
   */
  async function loadNamespace(namespace) {
    const cacheKey = `${currentLanguage}:${namespace}`;

    // Skip if already loaded
    if (loadedNamespaces[cacheKey]) {
      return Promise.resolve();
    }

    const localePath = `/locales/${currentLanguage}/${namespace}.json`;
    console.log(`Loading namespace: ${namespace} from ${localePath}`);

    try {
      const response = await fetch(localePath);

      if (response.ok) {
        const translations = await response.json();

        // Кэшировать весь объект перевода вместо отдельных ключей
        translationCache[`${currentLanguage}:${namespace}`] = translations;

        // Для обратной совместимости также сохранять плоскую версию
        for (const [k, v] of Object.entries(flattenObject(translations, namespace))) {
          translationCache[`${currentLanguage}:${namespace}:${k}`] = v;
        }

        loadedNamespaces[cacheKey] = true;
        console.log(`Успешно загружен namespace: ${namespace} с ${Object.keys(translations).length} ключами`);
        return translations;
      } else {
        console.warn(`Failed to load namespace ${namespace}: HTTP ${response.status}`);
        loadedNamespaces[cacheKey] = false; // Mark as checked but not found
        return null;
      }
    } catch (error) {
      console.error(`Error loading namespace ${namespace}: ${error.message}`);
      loadedNamespaces[cacheKey] = false; // Mark as checked but not found

      // Fallback to default language translations if available
      const fallbackTranslations = translationCache[`${defaultLanguage}:${namespace}`];
      if (fallbackTranslations) {
        console.log(`Using fallback translations for namespace: ${namespace}`);
        for (const [k, v] of Object.entries(fallbackTranslations)) {
          translationCache[`${currentLanguage}:${namespace}:${k}`] = v;
        }
        loadedNamespaces[cacheKey] = true;
        return fallbackTranslations;
      }
      return null;
    }
  }

  /**
   * Extract namespace from translation key
   * @param {string} key - Translation key
   * @returns {string} - Namespace
   */
  function getNamespaceFromKey(key) {
    const parts = key.split(':');
    return parts.length > 1 ? parts[0] : 'common';
  }

  /**
   * Function to get a translation value
   * @param {string} key - Translation key
   * @param {Object} options - Options for interpolation
   * @returns {string} - Translated text or original key
   */
  function getTranslation(key, options = {}) {
    // Early returns for initialization checks and default language
    if (!initialized || currentLanguage === defaultLanguage || !key) {
      return key;
    }

    // Parse namespace
    const parts = key.split(':');
    const namespace = parts.length > 1 ? parts[0] : 'common';
    const actualKey = parts.length > 1 ? parts.slice(1).join(':') : key;

    // Get the base namespace object from cache
    const baseNamespaceKey = `${currentLanguage}:${namespace}`;

    // Check if namespace is loaded
    if (!loadedNamespaces[baseNamespaceKey]) {
      // Schedule loading the namespace
      // ...existing loading code...
      return key;
    }

    // Now handle nested paths using dot notation
    const pathParts = actualKey.split('.');

    // Start with the full namespace object
    let translation = null;
    let currentObject = null;

    // First get the base object for this namespace
    for (const cacheKey in translationCache) {
      if (cacheKey.startsWith(`${currentLanguage}:${namespace}:`)) {
        const objectKey = cacheKey.substring(`${currentLanguage}:${namespace}:`.length);
        if (objectKey === pathParts[0]) {
          currentObject = translationCache[cacheKey];
          break;
        }
      }
    }

    // Navigate through the nested structure
    if (currentObject) {
      // For single-level keys
      if (pathParts.length === 1) {
        translation = currentObject;
      }
      // For multi-level keys
      else {
        let current = currentObject;
        // Start from index 1 as we already matched the first part
        for (let i = 1; i < pathParts.length; i++) {
          if (current && typeof current === 'object' && current[pathParts[i]] !== undefined) {
            current = current[pathParts[i]];
          } else {
            current = null;
            break;
          }
        }
        translation = current;
      }
    }

    // If translation found, return it with interpolation
    if (translation !== null) {
      return interpolate(translation, options);
    }

    // Request API translation as fallback (if not in progress)
    const cacheKey = `${currentLanguage}:${namespace}:${actualKey}`;
    if (!pendingTranslations.has(cacheKey)) {
      requestApiTranslation(key, namespace, cacheKey);
    }

    return key;
  }

  /**
   * Request translation from API
   * @param {string} key - Translation key
   * @param {string} namespace - Namespace
   * @param {string} cacheKey - Cache key
   * @param {boolean} isHtml - Whether content is HTML
   */
  function requestApiTranslation(key, namespace, cacheKey, isHtml = false) {
    if (pendingTranslations.has(cacheKey)) return;

    // Find an element with this key
    const elementWithKey = document.querySelector(`[data-i18n="${key}"]`);

    // Get the text content or use a fallback
    let englishText;
    if (elementWithKey) {
      englishText = elementWithKey.textContent.trim();
    } else {
      // Try to get the text from the default language namespace
      const defaultNamespace = namespace || 'common';
      const defaultKey = key.split(':').pop(); // Get the last part of the key
      const defaultTranslation = translationCache[`${defaultLanguage}:${defaultNamespace}:${defaultKey}`];
      englishText = defaultTranslation || key;
    }

    // If the text is still a key, try to get the actual text
    if (englishText.startsWith('rma:')) {
      const parts = englishText.split(':');
      const ns = parts[0];
      const actualKey = parts.slice(1).join(':');
      const defaultTranslation = translationCache[`${defaultLanguage}:${ns}:${actualKey}`];
      if (defaultTranslation) {
        englishText = defaultTranslation;
      }
    }

    console.log(`Key "${key}" not found in static files, requesting API translation ${englishText}`);
    pendingTranslations.add(cacheKey);

    // В функции requestApiTranslation модифицировать блок fetch с обработкой ошибки:
    fetch('/api/translate', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        text: englishText,
        targetLang: currentLanguage,
        context: namespace,
        htmlContent: isHtml
      })
    })
      .then(response => response.json())
      .then(data => {
        // Существующий код...
      })
      .catch(error => {
        console.error('Ошибка API перевода:', error);
        pendingTranslations.delete(cacheKey);

        // Добавить проверку на офлайн режим
        if (!navigator.onLine) {
          // Если офлайн, не пытаться повторять запросы к API
          console.log('Устройство офлайн, прекращаем попытки перевода');
          translationRetryCount[cacheKey] = MAX_RETRIES;
        }
      });
  }

  /**
   * Update all DOM elements using a translation key
   * @param {string} key - Translation key
   * @param {string} translation - Translated text
   * @param {boolean} isHtml - Whether content is HTML
   */
  function updateElementsWithKey(key, translation, isHtml = false) {
    console.log(`Updating elements with key: ${key}`);

    // Find all elements with this key
    const elements = document.querySelectorAll(`[data-i18n="${key}"]`);
    console.log(`Found ${elements.length} elements with key ${key}`);

    elements.forEach(el => {
      if (isHtml) {
        el.innerHTML = translation;
      } else {
        el.textContent = translation;
      }
      el.classList.remove('i18n-loading');
      console.log(`Updated element with key ${key}`);
    });

    // Also check attributes
    const attrElements = document.querySelectorAll('[data-i18n-attr]');
    console.log(`Checking ${attrElements.length} elements with data-i18n-attr`);

    attrElements.forEach(el => {
      try {
        const attrsMap = JSON.parse(el.getAttribute('data-i18n-attr'));
        for (const [attr, attrKey] of Object.entries(attrsMap)) {
          if (attrKey === key) {
            el.setAttribute(attr, translation);
            el.classList.remove('i18n-loading');
            console.log(`Updated attribute ${attr} for element with key ${key}`);
          }
        }
      } catch (e) {
        console.error('Error updating attribute translation:', e);
      }
    });

    // Update HTML content if applicable
    if (isHtml) {
      const htmlElements = document.querySelectorAll(`[data-i18n-html="${key}"]`);
      console.log(`Found ${htmlElements.length} elements with HTML content for key ${key}`);

      htmlElements.forEach(el => {
        el.innerHTML = translation;
        el.classList.remove('i18n-loading');
        console.log(`Updated HTML content for element with key ${key}`);
      });
    }
  }

  /**
   * Interpolate variables in a string
   * @param {string} text - Template with variables
   * @param {Object} options - Variables
   * @returns {string} - Interpolated string
   */
  function interpolate(text, options) {
    if (!options || typeof text !== 'string') {
      return text;
    }

    // Replace {{var}} with values from options
    return text.replace(/\{\{([^}]+)\}\}/g, (match, key) => {
      return options[key] !== undefined ? options[key] : match;
    });
  }

  /**
   * Helper function to set cookie
   * @param {string} name - Cookie name
   * @param {string} value - Cookie value
   * @param {number} days - Days until expiration
   */
  function setCookie(name, value, days) {
    let expires = '';
    if (days) {
      const date = new Date();
      date.setTime(date.getTime() + (days * 24 * 60 * 60 * 1000));
      expires = '; expires=' + date.toUTCString();
    }
    document.cookie = name + '=' + encodeURIComponent(value) + expires + '; path=/';
  }

  /**
   * Helper function to get cookie
   * @param {string} name - Cookie name
   * @returns {string|null} - Cookie value or null if not found
   */
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
      console.log(`Режим отладки i18n ${this.debugMode ? 'включен' : 'выключен'}`);
      if (this.debugMode) {
        this.checkTranslationStructure();
      }
    },
    checkTranslationStructure: function () {
      console.log('===== ЗАГРУЖЕННЫЕ ПЕРЕВОДЫ =====');
      console.log('Кэш переводов:', translationCache);
      console.log('Загруженные пространства имён:', loadedNamespaces);
      console.log('Текущий язык:', currentLanguage);
    },
    init,
    changeLanguage,
    getCurrentLanguage: () => currentLanguage || defaultLanguage,
    updatePageTranslations,
    translateDynamicElement: (element, context = '') => {
      if (!element) return Promise.resolve();
      return translateDynamicElement(element, context);
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