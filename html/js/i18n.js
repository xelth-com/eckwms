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

/**
 * Find untranslated elements in DOM with improved detection
 * @returns {Object} - Object with untranslated elements and count
 */
function findUntranslatedElements() {
  const untranslatedElements = {
    standard: [],
    attributes: [],
    html: []
  };

  console.log(`[i18n] Checking for untranslated elements (current language: ${currentLanguage})`);

  // Check standard text elements
  document.querySelectorAll('[data-i18n]').forEach(el => {
    const key = el.getAttribute('data-i18n');
    if (!key) return;

    // Get the text content to check if it looks untranslated
    const content = el.textContent.trim();
    
    // A better check for untranslated content:
    // 1. If content is empty, it's definitely untranslated
    // 2. If content exactly matches the key or the last part of the key, it's untranslated
    const keyParts = key.split(':');
    const shortKey = keyParts[keyParts.length - 1];
    const keyLastPart = shortKey.split('.').pop(); // Get last part after the dot
    
    let needsTranslation = false;
    
    if (content === '' || content === key || content === shortKey || content === keyLastPart) {
      // Empty or showing the key - definitely needs translation
      needsTranslation = true;
    }

    // Don't mark already translated content as needing translation
    // This is the key fix - only add untranslated elements
    if (needsTranslation) {
      untranslatedElements.standard.push({
        element: el,
        key: key
      });
      console.log(`[i18n] Found untranslated element: ${key}, content="${content}"`);
    }
  });

  // Check attribute translations with data-i18n-attr
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
        // Similar logic to above, for attribute translations
        const keyParts = key.split(':');
        const shortKey = keyParts[keyParts.length - 1];
        const attrValue = el.getAttribute(attr) || '';
        
        // Only mark as untranslated if showing empty value or the key itself
        if (attrValue === '' || attrValue === key || attrValue === shortKey) {
          untranslatedElements.attributes.push({
            element: el,
            attribute: attr,
            key: key
          });
        }
      }
    } catch (error) {
      console.error('Error checking attribute translation:', error);
    }
  });

  // Count total untranslated elements
  const totalUntranslated =
    untranslatedElements.standard.length +
    untranslatedElements.attributes.length +
    untranslatedElements.html.length;

  console.log(`[i18n] Total untranslated elements: ${totalUntranslated}`);

  return {
    elements: untranslatedElements,
    count: totalUntranslated
  };
}

/**
 * Check for untranslated elements and schedule retry with improved logic
 */
function checkAndScheduleRetry() {
  // Cancel any existing retry timer
  if (retryTimerId) {
    clearTimeout(retryTimerId);
    retryTimerId = null;
  }
  
  // Find untranslated elements
  const untranslated = findUntranslatedElements();
  
  // If we have no untranslated elements, we're done
  if (untranslated.count === 0) {
    console.log(`[i18n] Translation check complete - all elements translated`);
    retryCount = 0;
    return;
  }
  
  // If we've reached max retries, log the elements that are still untranslated
  if (retryCount >= MAX_RETRY_ATTEMPTS) {
    console.log(`[i18n] Translation check complete - ${untranslated.count} untranslated items remain after ${retryCount} attempts`);
    
    // In development mode, log details about untranslated elements
    if (window.APP_CONFIG?.NODE_ENV === 'development') {
      console.group('Untranslated elements after max retries:');
      
      // Log standard elements
      untranslated.elements.standard.forEach(({ element, key }) => {
        console.log(`- Standard: "${key}" (content: "${element.textContent.substring(0, 30)}...")`);
      });
      
      // Log attribute elements
      untranslated.elements.attributes.forEach(({ element, attribute, key }) => {
        console.log(`- Attribute: "${key}" for attribute "${attribute}" on element`, element);
      });
      
      // Log HTML elements
      untranslated.elements.html.forEach(({ element, key }) => {
        console.log(`- HTML: "${key}" (inner HTML: "${element.innerHTML.substring(0, 30)}...")`);
      });
      
      console.groupEnd();
    }
    
    retryCount = 0;
    return;
  }
  
  // If we need to retry, schedule with proper delay
  const delay = RETRY_INTERVALS[retryCount];
  
  console.log(`[i18n] Scheduling retry attempt #${retryCount + 1} in ${delay}ms for ${untranslated.count} untranslated elements`);
  
  // Create missing namespace list
  const namespacesToLoad = new Set();
  
  // Analyze untranslated elements to determine which namespaces to load
  untranslated.elements.standard.forEach(({ key }) => {
    const parts = key.split(':');
    if (parts.length > 1) {
      namespacesToLoad.add(parts[0]);
    } else {
      // If no explicit namespace, add all standard namespaces
      namespacesToLoad.add('rma');
      namespacesToLoad.add('auth');
      namespacesToLoad.add('dashboard');
    }
  });
  
  // Always add common namespace
  namespacesToLoad.add('common');
  
  // Log identified namespaces in development
  if (window.APP_CONFIG?.NODE_ENV === 'development') {
    console.log(`[i18n] Identified ${namespacesToLoad.size} namespaces to load: ${Array.from(namespacesToLoad).join(', ')}`);
  }
  
  // Schedule retry with proper delay
  retryTimerId = setTimeout(async () => {
    try {
      // Load specifically identified namespaces with force=true
      for (const namespace of namespacesToLoad) {
        await loadNamespace(currentLanguage, namespace, '', true);
      }
      
      // Increment retry count for next attempt
      retryCount++;
      
      // Force update translations
      updatePageTranslations();
      
      // Only continue if we have remaining untranslated elements and haven't hit max attempts
      if (retryCount < MAX_RETRY_ATTEMPTS) {
        // Schedule next check after a short delay to allow processing
        setTimeout(() => {
          checkAndScheduleRetry();
        }, 100);
      }
    } catch (error) {
      console.error('Error during translation retry:', error);
      
      // Still increment retry count and continue
      retryCount++;
      checkAndScheduleRetry();
    }
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
 * Asynchronous module initialization with priority for app-language header
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

  // Determine current language with priorities that match the server config:
  // 1. Custom app-language header if available via meta tag
  // 2. URL query parameter "lang"
  // 3. Previously stored language in cookie
  // 4. Browser language
  // 5. Default language

  // Check if app-language was provided via meta tag
  const appLanguageMeta = document.querySelector('meta[name="app-language"]');
  let detectedLanguage = null;
  
  if (appLanguageMeta && appLanguageMeta.content) {
    detectedLanguage = appLanguageMeta.content;
    devLog(`Found app-language from meta tag: ${detectedLanguage}`);
  }

  // Check URL query parameter
  if (!detectedLanguage) {
    const urlParams = new URLSearchParams(window.location.search);
    if (urlParams.has('lang')) {
      detectedLanguage = urlParams.get('lang');
      devLog(`Found language from URL query: ${detectedLanguage}`);
    }
  }

  // Check cookie
  if (!detectedLanguage) {
    detectedLanguage = getCookie('i18next');
    if (detectedLanguage) {
      devLog(`Found language from cookie: ${detectedLanguage}`);
    }
  }

  // Check localStorage
  if (!detectedLanguage) {
    detectedLanguage = localStorage.getItem('i18nextLng');
    if (detectedLanguage) {
      devLog(`Found language from localStorage: ${detectedLanguage}`);
    }
  }

  // Fall back to browser language or default
  if (!detectedLanguage) {
    detectedLanguage = navigator.language.split('-')[0] || defaultLanguage;
    devLog(`Using browser language or default: ${detectedLanguage}`);
  }

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
  
  // Also set app-language header for future fetch requests
  if (window.fetch) {
    const originalFetch = window.fetch;
    window.fetch = function(url, options = {}) {
      if (!options.headers) {
        options.headers = {};
      }
      
      // Add app-language header to all fetch requests
      if (typeof options.headers.set === 'function') {
        options.headers.set('app-language', currentLanguage);
      } else {
        options.headers['app-language'] = currentLanguage;
      }
      
      return originalFetch.call(this, url, options);
    };
    devLog(`Monkey-patched fetch to add app-language header: ${currentLanguage}`);
  }

  // Synchronize with window.language for SVG buttons
  window.language = currentLanguage;

  // Synchronize SVG language masks
  syncLanguageMasks();

  // Rest of the initialization function remains the same...
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
 * Preload common namespaces with improved error handling and logging
 * @param {boolean} force - Whether to force reload of already loaded namespaces
 * @returns {Promise<void>}
 */
async function preloadCommonNamespaces(force = false) {
  if (currentLanguage === defaultLanguage) {
    devLog("Using default language, skipping namespace preload");
    return;
  }

  // Make sure to preload ALL possible namespaces where translations might be found
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
            
            // Try to load from localStorage as fallback
            if (tryLoadFromLocalStorage(currentLanguage, namespace)) {
              devLog(`Successfully loaded ${namespace} from localStorage fallback`);
              return true;
            }
            
            return false;
          }

          const data = await response.json();
          devLog(`Successfully loaded namespace ${namespace} for ${currentLanguage}`);

          // Cache all translations from this namespace
          const flattenedData = flattenTranslations(data);
          
          // Log some statistics in dev mode
          if (window.APP_CONFIG?.NODE_ENV === 'development') {
            const count = Object.keys(flattenedData).length;
            devLog(`Loaded ${count} translations for ${namespace}`);
            
            // Log the first few translations as examples
            const examples = Object.entries(flattenedData).slice(0, 3);
            examples.forEach(([k, v]) => {
              devLog(`  ${k}: "${v.substring(0, 30)}${v.length > 30 ? '...' : ''}"`);
            });
          }
          
          for (const [key, value] of Object.entries(flattenedData)) {
            const fullCacheKey = `${currentLanguage}:${namespace}:${key}`;
            translationCache[fullCacheKey] = value;
          }

          // Mark namespace as loaded
          loadedNamespaces[cacheKey] = true;
          
          // Save to localStorage for offline/faster loading next time
          try {
            localStorage.setItem(`i18n_${currentLanguage}_${namespace}`, JSON.stringify(data));
          } catch (storageError) {
            console.warn(`Failed to save namespace to localStorage:`, storageError);
            // Try to clean up storage if it might be full
            if (storageError.name === 'QuotaExceededError') {
              cleanupLocalStorage();
            }
          }
          
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
      
      // If some namespaces failed to load, try loading from localStorage
      if (loadedCount < loadPromises.length) {
        devLog(`Some namespaces failed to load, trying localStorage fallbacks`);
      }
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
 * Get translation for a key with improved namespace resolution
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
  const explicitNamespace = parts.length > 1;
  const namespace = explicitNamespace ? parts[0] : 'common';
  const actualKey = explicitNamespace ? parts.slice(1).join(':') : key;

  // Build cache key
  const cacheKey = `${currentLanguage}:${namespace}:${actualKey}`;

  console.log(`[i18n] Looking for translation: ${cacheKey}`);

  // Check if translation exists in cache
  if (translationCache[cacheKey]) {
    console.log(`[i18n] Found translation in cache for ${cacheKey}: "${translationCache[cacheKey].substring(0, 30)}..."`);
    return interpolate(translationCache[cacheKey], options);
  }

  // If no explicit namespace was provided, also check in other namespaces
  if (!explicitNamespace) {
    // Priority list of namespaces to check (add more if needed)
    const namespacesToCheck = ['rma', 'dashboard', 'auth'];
    
    for (const ns of namespacesToCheck) {
      const altCacheKey = `${currentLanguage}:${ns}:${actualKey}`;
      
      console.log(`[i18n] Also checking in namespace: ${ns}, key: ${altCacheKey}`);
      
      if (translationCache[altCacheKey]) {
        console.log(`[i18n] Found translation in alternative namespace ${ns}`);
        return interpolate(translationCache[altCacheKey], options);
      }
    }
    
    console.log(`[i18n] Translation not found in any namespace for ${actualKey}`);
  } else {
    console.log(`[i18n] Translation not found in cache for ${cacheKey}`);
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
 * Update translations on the current page with improved element tracking
 */
function updatePageTranslations() {
  // Skip if using default language or not initialized
  if (currentLanguage === defaultLanguage || !initialized) {
    console.log('Skipping translation update - using default language or not initialized');
    return;
  }

  console.log(`[i18n] Updating page translations for language: ${currentLanguage}`);
  console.log(`[i18n] Cache contains ${Object.keys(translationCache).length} translations`);

  // Track successfully translated elements
  const updatedElements = {
    standard: 0,
    attributes: 0,
    html: 0
  };

  try {
    // Count elements before updating for logging
    const tagCount = document.querySelectorAll('[data-i18n]').length;
    const attrCount = document.querySelectorAll('[data-i18n-attr]').length;
    const htmlCount = document.querySelectorAll('[data-i18n-html]').length;

    console.log(`Found ${tagCount} standard, ${attrCount} attribute, and ${htmlCount} HTML translation tags`);

    // 1. Process all elements with data-i18n attribute with namespace fallbacks
    document.querySelectorAll('[data-i18n]').forEach(el => {
      try {
        const key = el.getAttribute('data-i18n');
        if (!key) return;

        // Try finding translation in multiple namespaces if needed
        const parts = key.split(':');
        const explicitNamespace = parts.length > 1;
        const primaryNamespace = explicitNamespace ? parts[0] : 'common';
        const actualKey = explicitNamespace ? parts.slice(1).join(':') : key;
        
        let translation = null;
        let foundNamespace = null;
        
        // Check primary namespace first
        const primaryCacheKey = `${currentLanguage}:${primaryNamespace}:${actualKey}`;
        if (translationCache[primaryCacheKey]) {
          translation = translationCache[primaryCacheKey];
          foundNamespace = primaryNamespace;
        } 
        // If no explicit namespace, also check alternative namespaces
        else if (!explicitNamespace) {
          const namespacesToCheck = ['rma', 'dashboard', 'auth'];
          for (const ns of namespacesToCheck) {
            const altCacheKey = `${currentLanguage}:${ns}:${actualKey}`;
            if (translationCache[altCacheKey]) {
              translation = translationCache[altCacheKey];
              foundNamespace = ns;
              break;
            }
          }
        }

        // Log which namespace we found the translation in (for debugging)
        if (translation) {
          console.log(`[i18n] Found translation for ${key} in ${foundNamespace} namespace: "${translation.substring(0, 30)}${translation.length > 30 ? '...' : ''}"`);
        } else {
          console.log(`[i18n] No translation found for ${key} in any namespace`);
        }

        // Only update if we have a non-null translation and it's different from current content
        if (translation && el.textContent !== translation) {
          el.textContent = translation;
          updatedElements.standard++;
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
          // Use the same namespace fallback logic as for standard elements
          const parts = key.split(':');
          const explicitNamespace = parts.length > 1;
          const primaryNamespace = explicitNamespace ? parts[0] : 'common';
          const actualKey = explicitNamespace ? parts.slice(1).join(':') : key;
          
          let translation = null;
          
          // Check primary namespace first
          const primaryCacheKey = `${currentLanguage}:${primaryNamespace}:${actualKey}`;
          if (translationCache[primaryCacheKey]) {
            translation = translationCache[primaryCacheKey];
          } 
          // If no explicit namespace, also check alternative namespaces
          else if (!explicitNamespace) {
            const namespacesToCheck = ['rma', 'dashboard', 'auth'];
            for (const ns of namespacesToCheck) {
              const altCacheKey = `${currentLanguage}:${ns}:${actualKey}`;
              if (translationCache[altCacheKey]) {
                translation = translationCache[altCacheKey];
                break;
              }
            }
          }

          // Only update if translation found and different from current attribute
          if (translation && el.getAttribute(attr) !== translation) {
            el.setAttribute(attr, translation);
            updatedElements.attributes++;
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

        // Use the same namespace fallback logic
        const parts = key.split(':');
        const explicitNamespace = parts.length > 1;
        const primaryNamespace = explicitNamespace ? parts[0] : 'common';
        const actualKey = explicitNamespace ? parts.slice(1).join(':') : key;
        
        let translation = null;
        
        // Check primary namespace first
        const primaryCacheKey = `${currentLanguage}:${primaryNamespace}:${actualKey}`;
        if (translationCache[primaryCacheKey]) {
          translation = translationCache[primaryCacheKey];
        } 
        // If no explicit namespace, also check alternative namespaces
        else if (!explicitNamespace) {
          const namespacesToCheck = ['rma', 'dashboard', 'auth'];
          for (const ns of namespacesToCheck) {
            const altCacheKey = `${currentLanguage}:${ns}:${actualKey}`;
            if (translationCache[altCacheKey]) {
              translation = translationCache[altCacheKey];
              break;
            }
          }
        }

        // Only update if translation found and different from current HTML
        if (translation && el.innerHTML !== translation) {
          el.innerHTML = translation;
          updatedElements.html++;
        }
      } catch (error) {
        console.error('Error updating HTML translation for element:', el, error);
      }
    });

    console.log(`[i18n] Updated ${updatedElements.standard} standard, ${updatedElements.attributes} attribute, and ${updatedElements.html} HTML elements`);

    // After updating translations, check for any remaining untranslated elements
    if (!retryTimerId && retryCount === 0) {
      const untranslated = findUntranslatedElements();
      if (untranslated.count > 0) {
        console.log(`[i18n] Starting retry checks for ${untranslated.count} remaining untranslated elements`);
        checkAndScheduleRetry();
      } else {
        console.log(`[i18n] All elements are now translated!`);
      }
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