// html/js/i18n.js

/**
 * Client-side multilanguage support module
 */
(function () {
  // Default language (fallback)
  let defaultLanguage = window.APP_CONFIG?.DEFAULT_LANGUAGE || 'en';

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

  // Track retry attempts
  let retryCount = 0;
  // Retry intervals in milliseconds (3s, 7s, 15s, 25s, 35s, 50s, 70s)
  const RETRY_INTERVALS = [3000, 7000, 15000, 25000, 35000, 50000, 70000];
  // Maximum number of retry attempts
  const MAX_RETRY_ATTEMPTS = RETRY_INTERVALS.length;
  // Retry timer ID for cleaning up
  let retryTimerId = null;

  // Utility function for dev-only console logging
  function devLog(...args) {
    if (window.APP_CONFIG?.NODE_ENV === 'development') {
      console.log(...args);
    }
  }

  // Function to find untranslated elements in DOM
  function findUntranslatedElements() {
    const untranslatedElements = {
      standard: [],
      attributes: [],
      html: []
    };

    console.log(`[i18n] Проверка непереведенных элементов (текущий язык: ${currentLanguage})`);
    console.log(`[i18n] Кеш переводов имеет ${Object.keys(translationCache).length} записей`);

    // Check standard text elements
    document.querySelectorAll('[data-i18n]').forEach(el => {
      const key = el.getAttribute('data-i18n');

      // Parse the key to handle namespaces
      const parts = key.split(':');
      const namespace = parts.length > 1 ? parts[0] : 'common';
      const actualKey = parts.length > 1 ? parts.slice(1).join(':') : key;

      // Build cache key for lookup
      const cacheKey = `${currentLanguage}:${namespace}:${actualKey}`;

      // Check if translation exists in cache
      const hasTranslation = translationCache[cacheKey] !== undefined;

      console.log(`[i18n] Проверка элемента: key=${key}, cacheKey=${cacheKey}, hasTranslation=${hasTranslation}, content="${el.textContent.substring(0, 30)}..."`);

      // ИСПРАВЛЕНО: Добавлено сравнение с ключом
      if (!hasTranslation || el.textContent === key || el.textContent.trim() === '') {
        untranslatedElements.standard.push({
          element: el,
          key: key
        });
        console.log(`[i18n] Добавлен элемент в список непереведенных: ${key}`);
      }
    });

    // Остальная часть функции без изменений...

    // Count total untranslated elements
    const totalUntranslated =
      untranslatedElements.standard.length +
      untranslatedElements.attributes.length +
      untranslatedElements.html.length;

    console.log(`[i18n] Всего непереведенных элементов: ${totalUntranslated}`);

    return {
      elements: untranslatedElements,
      count: totalUntranslated
    };
  }

  function checkAndScheduleRetry() {
    // Cancel any existing retry timer
    if (retryTimerId) {
      clearTimeout(retryTimerId);
      retryTimerId = null;
    }

    console.log(`[i18n] Запуск проверки переводов (попытка #${retryCount + 1})`);

    // Find untranslated elements
    const untranslated = findUntranslatedElements();

    // If we have no untranslated elements, we're done
    if (untranslated.count === 0) {
      console.log("[i18n] Все элементы переведены успешно!");
      retryCount = 0;
      return;
    }

    // Detailed logging of untranslated elements
    console.log("[i18n] Непереведенные элементы:");
    console.log("Стандартные:", untranslated.elements.standard.map(e => e.key));
    console.log("Атрибуты:", untranslated.elements.attributes.map(e => `${e.attribute}=${e.key}`));
    console.log("HTML:", untranslated.elements.html.map(e => e.key));

    // Get delay for this retry
    const delay = RETRY_INTERVALS[retryCount];

    console.log(`[i18n] Планирование повторной попытки #${retryCount + 1} через ${delay}мс для ${untranslated.count} непереведенных элементов`);

    // Schedule retry
    retryTimerId = setTimeout(() => {
      console.log(`[i18n] Выполнение повторной попытки #${retryCount + 1}`);
      // Reload translations and update page
      preloadCommonNamespaces(true)
        .then(() => {
          // Force update translations
          updatePageTranslations();

          // Increment retry count for next attempt
          retryCount++;

          // Check again for any remaining untranslated elements
          checkAndScheduleRetry();
        })
        .catch(error => {
          console.error("[i18n] Ошибка во время повторной попытки перевода:", error);
          // Even on error, continue with retry schedule
          retryCount++;
          checkAndScheduleRetry();
        });
    }, delay);
  }

  /**
   * Flattens a nested translation object into a flat structure with dot notation
   * Example: { form: { title: "Hello" } } becomes { "form.title": "Hello" }
   * @param {Object} obj - The nested object to flatten
   * @param {string} prefix - The prefix for keys (used in recursion)
   * @returns {Object} - A flattened object with dot notation keys
   */
  function flattenTranslations(obj, prefix = '') {
    return Object.keys(obj).reduce((acc, key) => {
      const prefixedKey = prefix ? `${prefix}.${key}` : key;

      if (typeof obj[key] === 'object' && obj[key] !== null && !Array.isArray(obj[key])) {
        Object.assign(acc, flattenTranslations(obj[key], prefixedKey));
      } else {
        acc[prefixedKey] = obj[key];
      }

      return acc;
    }, {});
  }

  /**
   * Alias for flattenTranslations - both functions serve the same purpose
   * @param {Object} obj - The nested object to flatten
   * @param {string} prefix - The prefix for keys (used in recursion)
   * @returns {Object} - A flattened object with dot notation keys
   */
  function flattenObject(obj, prefix = '') {
    return flattenTranslations(obj, prefix);
  }

  /**
   * Attempts to load translations from localStorage
   * @param {string} language - Language code
   * @param {string} namespace - Namespace to load
   * @returns {boolean} - Whether loading from localStorage was successful
   */
  function tryLoadFromLocalStorage(language, namespace) {
    try {
      const cacheKey = `${language}:${namespace}`;
      const localData = localStorage.getItem(`i18n_${language}_${namespace}`);

      if (localData) {
        // Try to parse the data
        const parsedData = JSON.parse(localData);

        // Process translations and add to cache
        const flattened = flattenObject(parsedData);

        for (const [key, value] of Object.entries(flattened)) {
          const fullCacheKey = `${language}:${namespace}:${key}`;
          translationCache[fullCacheKey] = value;
        }

        // Mark namespace as loaded
        loadedNamespaces[cacheKey] = true;

        devLog(`Loaded namespace ${namespace} from localStorage`);
        return true;
      }
    } catch (error) {
      console.warn(`Failed to load namespace ${namespace} from localStorage:`, error);
    }

    return false;
  }

  /**
   * Asynchronous module initialization
   */
  async function init() {
    devLog("Initializing i18n...");

    // Check DOM state
    if (document.readyState !== 'complete' && document.readyState !== 'interactive') {
      devLog("DOM not ready, waiting for load...");
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
      devLog(`Language ${detectedLanguage} is not supported, using ${defaultLanguage}`);
      detectedLanguage = defaultLanguage;
    }

    // Set the current language
    currentLanguage = detectedLanguage;
    devLog(`Set language: ${currentLanguage}`);

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
        devLog('Removed cache busting parameter from URL');
      }, 2000); // 2 seconds delay to ensure page loaded
    }

    // Set initialization flag
    initialized = true;

    // Set up mutation observer to translate dynamically added content
    if (window.MutationObserver && currentLanguage !== defaultLanguage) {
      const observer = new MutationObserver((mutations) => {
        // Check if any mutations added nodes with data-i18n attributes
        let hasTranslatableContent = false;

        for (const mutation of mutations) {
          if (mutation.type === 'childList' && mutation.addedNodes.length) {
            for (const node of mutation.addedNodes) {
              // Only process element nodes
              if (node.nodeType === Node.ELEMENT_NODE) {
                // Check if this node or its children have data-i18n attributes
                if (node.querySelector('[data-i18n], [data-i18n-attr], [data-i18n-html]') ||
                  node.hasAttribute('data-i18n') ||
                  node.hasAttribute('data-i18n-attr') ||
                  node.hasAttribute('data-i18n-html')) {
                  hasTranslatableContent = true;
                  break;
                }
              }
            }
          }

          if (hasTranslatableContent) break;
        }

        // If we found translatable content, update translations
        if (hasTranslatableContent) {
          devLog('Detected new translatable content, updating translations');
          updatePageTranslations();
        }
      });

      // Start observing the document
      observer.observe(document.documentElement, {
        childList: true,
        subtree: true
      });

      devLog('Translation mutation observer started');
    }

    // Generate event that i18n is initialized
    document.dispatchEvent(new CustomEvent('i18n:initialized', {
      detail: { language: currentLanguage }
    }));

    // Force update translations when the page is fully loaded
    window.addEventListener('load', () => {
      if (initialized && currentLanguage !== defaultLanguage) {
        devLog("Page fully loaded, forcing translation update");

        // Do the first update immediately
        updatePageTranslations();

        // Do another update after a short delay to catch any late-rendered elements
        setTimeout(() => {
          updatePageTranslations();

          // Start checking for untranslated elements
          checkAndScheduleRetry();

          // Log translation status
          if (window.APP_CONFIG?.NODE_ENV === 'development') {
            const translated = document.querySelectorAll('[data-i18n]').length;
            devLog(`Translation status: ${translated} elements with data-i18n tags found`);
          }
        }, 500);
      }
    });

    // Also run updatePageTranslations when language changes
    document.addEventListener('languageChanged', (event) => {
      if (event.detail && event.detail.language !== defaultLanguage) {
        devLog(`Language changed to ${event.detail.language}, updating translations`);
        updatePageTranslations();
      }
    });

    devLog("i18n successfully initialized");
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
  * Preload common namespaces with improved error handling
  */
  async function preloadCommonNamespaces(force = false) {
    if (currentLanguage === defaultLanguage) {
      devLog("Using default language, skipping namespace preload");
      return;
    }

    const namespaces = ['common', 'auth', 'rma', 'dashboard'];
    devLog(`Preloading namespaces for ${currentLanguage}: ${namespaces.join(', ')}`);

    const loadPromises = [];

    for (const namespace of namespaces) {
      const cacheKey = `${currentLanguage}:${namespace}`;

      // Skip if already loaded and not forced
      if (loadedNamespaces[cacheKey] && !force) {
        devLog(`Namespace ${namespace} already loaded, skipping`);
        continue;
      }

      try {
        // Create a promise to load this namespace
        const loadPromise = (async () => {
          try {
            // Add cache busting parameter to prevent caching issues
            const timestamp = Date.now();
            const url = `/locales/${currentLanguage}/${namespace}.json?v=${timestamp}`;

            devLog(`Loading namespace from: ${url}`);
            const response = await fetch(url);

            if (!response.ok) {
              console.warn(`Failed to load namespace ${namespace} for ${currentLanguage}: ${response.status} ${response.statusText}`);
              return false;
            }

            const data = await response.json();
            devLog(`Successfully loaded namespace ${namespace} for ${currentLanguage}`);

            // Cache all translations from this namespace
            const flattenedData = flattenTranslations(data);
            for (const [key, value] of Object.entries(flattenedData)) {
              const fullCacheKey = `${currentLanguage}:${namespace}:${key}`;
              translationCache[fullCacheKey] = value;
            }

            // Mark namespace as loaded
            loadedNamespaces[cacheKey] = true;
            return true;
          } catch (error) {
            console.error(`Error loading namespace ${namespace} for ${currentLanguage}:`, error);
            return false;
          }
        })();

        loadPromises.push(loadPromise);
      } catch (error) {
        console.error(`Error setting up namespace load for ${namespace}:`, error);
      }
    }

    // Wait for all promises to complete
    if (loadPromises.length > 0) {
      try {
        const results = await Promise.all(loadPromises);
        const loadedCount = results.filter(success => success).length;
        devLog(`Preloaded ${loadedCount} of ${loadPromises.length} namespaces`);
      } catch (error) {
        console.error('Error in namespace preloading:', error);
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

    devLog(`i18n.changeLanguage: switching to ${lang}`);

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
    retryCount = 0;

    if (retryTimerId) {
      clearTimeout(retryTimerId);
      retryTimerId = null;
    }

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
        devLog("Switching to default language without page reload");
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

    devLog(`Checking translation files for ${currentLanguage}...`);

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
  
    // Добавляем лог для отображения попыток поиска
    console.log(`[i18n] Поиск перевода: ${cacheKey}`);
  
    // Check if translation exists in cache
    if (translationCache[cacheKey]) {
      console.log(`[i18n] Найден перевод в кеше для ${cacheKey}: "${translationCache[cacheKey].substring(0, 30)}..."`);
      return interpolate(translationCache[cacheKey], options);
    } else {
      console.log(`[i18n] Перевод в кеше не найден для ${cacheKey}`);
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

  // html/js/i18n.js - Улучшенная стратегия загрузки переводов
  async function loadTranslationFiles(language) {
    // Если нет cookies для отслеживания версий, установим их
    const versionKey = `i18n_ver_${language}`;
    let currentVersion = getCookie(versionKey) || '0';

    try {
      // Сначала запрашиваем метаданные версий для всех namespace
      const response = await fetch(`/locales/${language}/versions.json?v=${Date.now()}`);

      if (response.ok) {
        const versions = await response.json();
        const updatedNamespaces = [];

        // Для каждого namespace проверяем версию
        for (const [namespace, version] of Object.entries(versions)) {
          const namespaceVersion = localStorage.getItem(`i18n_${language}_${namespace}_ver`) || '0';

          // Если версия изменилась, добавляем в список для обновления
          if (version !== namespaceVersion) {
            updatedNamespaces.push(namespace);
            localStorage.setItem(`i18n_${language}_${namespace}_ver`, version);
          }
        }

        // Загружаем только измененные namespace
        if (updatedNamespaces.length > 0) {
          await Promise.all(updatedNamespaces.map(namespace =>
            loadNamespace(language, namespace, versions[namespace])
          ));
        }

        // Обновляем общую версию переводов
        setCookie(versionKey, versions.global || '1', 365);
      } else {
        // Если не получили метаданные, загружаем все основные namespace
        await Promise.all(['common', 'auth', 'rma', 'dashboard'].map(namespace =>
          loadNamespace(language, namespace)
        ));
      }
    } catch (error) {
      console.error('Error loading translation files:', error);
      // Fallback - загружаем базовый namespace
      await loadNamespace(language, 'common');
    }
  }

  // Оптимизированная загрузка отдельного namespace
  async function loadNamespace(language, namespace, version = '') {
    const cacheKey = `${language}:${namespace}`;

    // Пропускаем, если уже загружен с текущей версией
    if (loadedNamespaces[cacheKey] && !version) {
      return Promise.resolve();
    }

    try {
      // Добавляем версию для предотвращения кеширования устаревших данных
      const versionParam = version ? `?v=${version}` : `?t=${Date.now()}`;
      const response = await fetch(`/locales/${language}/${namespace}.json${versionParam}`);

      if (response.ok) {
        const translations = await response.json();

        // Оптимизированное добавление переводов в кеш
        batchAddToTranslationCache(language, namespace, translations);

        // Отмечаем namespace как загруженный
        loadedNamespaces[cacheKey] = true;

        // Сохраняем в localStorage для быстрой загрузки при следующем визите
        try {
          localStorage.setItem(`i18n_${language}_${namespace}`, JSON.stringify(translations));
        } catch (e) {
          // Если localStorage полный, очищаем менее важные данные
          cleanupLocalStorage();
        }
      }
    } catch (error) {
      console.error(`Failed to load namespace ${namespace} for ${language}:`, error);

      // Пробуем загрузить из localStorage
      tryLoadFromLocalStorage(language, namespace);
    }
  }

  // Оптимизированное добавление переводов в кеш
  function batchAddToTranslationCache(language, namespace, translations) {
    // Предварительно подготовим массив записей для обновления DOM
    const updatableElements = new Map();

    // Находим все элементы, которые могут использовать переводы из этого namespace
    document.querySelectorAll('[data-i18n]').forEach(el => {
      const key = el.getAttribute('data-i18n');
      if (key.startsWith(`${namespace}:`) || (!key.includes(':') && namespace === 'common')) {
        const actualKey = key.includes(':') ? key.split(':')[1] : key;
        if (!updatableElements.has(actualKey)) {
          updatableElements.set(actualKey, []);
        }
        updatableElements.get(actualKey).push(el);
      }
    });

    // Обновляем кеш и DOM элементы в одном проходе
    for (const [key, value] of Object.entries(flattenObject(translations))) {
      const cacheKey = `${language}:${namespace}:${key}`;
      translationCache[cacheKey] = value;

      // Если есть элементы для обновления с этим ключом, обновляем их
      if (updatableElements.has(key)) {
        updatableElements.get(key).forEach(el => {
          el.textContent = value;
        });
      }
    }
  }

  /**
   * Clean up localStorage when it's full
   */
  function cleanupLocalStorage() {
    try {
      // Get all i18n keys in localStorage
      const i18nKeys = [];
      for (let i = 0; i < localStorage.length; i++) {
        const key = localStorage.key(i);
        if (key.startsWith('i18n_')) {
          i18nKeys.push(key);
        }
      }

      // Remove half of them (oldest first - sort is just alphabetical here)
      const removeCount = Math.ceil(i18nKeys.length / 2);
      i18nKeys.sort().slice(0, removeCount).forEach(key => {
        localStorage.removeItem(key);
      });

      console.warn(`Cleaned up ${removeCount} translation entries from localStorage`);
    } catch (e) {
      console.error('Error cleaning up localStorage:', e);
    }
  }

  /**
   * Update translations on the current page with improved error handling
   */
  function updatePageTranslations() {
    // Skip if using default language or not initialized
    if (currentLanguage === defaultLanguage || !initialized) {
      devLog('Skipping translation update - using default language or not initialized');
      return;
    }

    console.log(`[i18n] Обновление переводов на странице для языка: ${currentLanguage}`);
    console.log(`[i18n] Кеш содержит ${Object.keys(translationCache).length} переводов`);

    try {
      // Count elements before updating for logging
      const tagCount = document.querySelectorAll('[data-i18n]').length;
      const attrCount = document.querySelectorAll('[data-i18n-attr]').length;
      const htmlCount = document.querySelectorAll('[data-i18n-html]').length;

      devLog(`Found ${tagCount} standard, ${attrCount} attribute, and ${htmlCount} HTML translation tags`);

      // 1. Process all elements with data-i18n attribute
      document.querySelectorAll('[data-i18n]').forEach(el => {
        try {
          const key = el.getAttribute('data-i18n');
          if (!key) return;

          // Log information about the element being processed in development
          if (window.APP_CONFIG?.NODE_ENV === 'development') {
            devLog(`Processing element with key: ${key}, current content: ${el.textContent.substring(0, 30)}`);
          }

          const translation = getTranslation(key);

          // Only update if we have a translation and it's different from the key
          if (translation && translation !== key) {
            // Log the translation being applied in development
            if (window.APP_CONFIG?.NODE_ENV === 'development') {
              devLog(`Applying translation for ${key}: ${translation.substring(0, 30)}`);
            }
            el.textContent = translation;
          }
        } catch (error) {
          console.error('Error updating translation for element:', el, error);
        }
      });

      // 2. Process attribute translations with data-i18n-attr
      document.querySelectorAll('[data-i18n-attr]').forEach(el => {
        try {
          const attrsJson = el.getAttribute('data-i18n-attr');
          if (!attrsJson) return;

          let attrsMap;
          try {
            attrsMap = JSON.parse(attrsJson);
          } catch (parseError) {
            console.error('Failed to parse data-i18n-attr JSON:', attrsJson, parseError);
            return;
          }

          for (const [attr, key] of Object.entries(attrsMap)) {
            const translation = getTranslation(key);

            if (translation && translation !== key) {
              el.setAttribute(attr, translation);
            }
          }
        } catch (error) {
          console.error('Error updating attribute translation for element:', el, error);
        }
      });

      // 3. Process HTML translations with data-i18n-html
      document.querySelectorAll('[data-i18n-html]').forEach(el => {
        try {
          const key = el.getAttribute('data-i18n-html');
          if (!key) return;

          const translation = getTranslation(key);

          if (translation && translation !== key) {
            el.innerHTML = translation;
          }
        } catch (error) {
          console.error('Error updating HTML translation for element:', el, error);
        }
      });

      // После обновления переводов проверяем необходимость запланировать дополнительные повторные попытки
      // Only do this if not in a retry already (prevent recursion)
      if (!retryTimerId && retryCount === 0) {
        console.log("[i18n] Запуск проверки необходимости повторных попыток");
        checkAndScheduleRetry();
      } else {
        console.log(`[i18n] Пропуск запуска новой проверки, retryTimerId=${!!retryTimerId}, retryCount=${retryCount}`);
      }
    } catch (error) {
      console.error('Error in updatePageTranslations:', error);
    }
  }

  // Update translations for elements with specific key
  function updateTranslationsForKey(key) {
    if (currentLanguage === defaultLanguage || !initialized) return;

    // Find all elements with this key
    document.querySelectorAll(`[data-i18n="${key}"]`).forEach(el => {
      const translation = getTranslation(key);
      if (translation && translation !== key) {
        el.textContent = translation;
      }
    });

    // Also check for attribute translations
    document.querySelectorAll('[data-i18n-attr]').forEach(el => {
      try {
        const attrsMap = JSON.parse(el.getAttribute('data-i18n-attr'));
        for (const [attr, attrKey] of Object.entries(attrsMap)) {
          if (attrKey === key) {
            const translation = getTranslation(key);
            if (translation && translation !== key) {
              el.setAttribute(attr, translation);
            }
          }
        }
      } catch (e) {
        console.error('Error parsing data-i18n-attr:', e);
      }
    });

    // And HTML content translations
    document.querySelectorAll(`[data-i18n-html="${key}"]`).forEach(el => {
      const translation = getTranslation(key);
      if (translation && translation !== key) {
        el.innerHTML = translation;
      }
    });
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
      devLog(`i18n debug mode ${this.debugMode ? 'enabled' : 'disabled'}`);
      if (this.debugMode) {
        this.checkTranslationStructure();
      }
    },
    checkTranslationStructure: function () {
      devLog('===== LOADED TRANSLATIONS =====');
      devLog('Translation cache:', translationCache);
      devLog('Loaded namespaces:', loadedNamespaces);
      devLog('Current language:', currentLanguage);
    },
    init,
    changeLanguage,
    getCurrentLanguage: () => currentLanguage || defaultLanguage,
    updatePageTranslations,
    translateDynamicElement: (element, context = '') => {
      if (!element) return Promise.resolve();

      // Skip if default language or not initialized
      if (window.i18n.getCurrentLanguage() === defaultLanguage || !window.i18n.isInitialized()) {
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
    loadTranslationFile, // Export for external use

    // Added method to start retry checking manually
    startRetryCheck: function () {
      // Reset retry count and clear any existing timer
      retryCount = 0;
      if (retryTimerId) {
        clearTimeout(retryTimerId);
        retryTimerId = null;
      }

      // Start checking for untranslated elements
      checkAndScheduleRetry();
    }
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
        devLog("i18n self-initializing on DOMContentLoaded");
        init();
      }
    });
  } else {
    // Check if initialization already happened
    if (!window.i18nInitStarted) {
      window.i18nInitStarted = true;
      devLog("i18n self-initializing immediately");
      init();
    }
  }
})();