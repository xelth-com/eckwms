// html/js/i18n.js

/**
 * Client-side multilanguage support module
 */
(function() {
  // Default language (will be updated during configuration loading)
  let defaultLanguage = 'en';
  
  // Current language (initially null, will be set after configuration loading)
  let currentLanguage = null;
  
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
  const translationCache = {};

  // Translation retry tracking
  const translationRetryCount = {};
  const MAX_RETRIES = 3;
  const RETRY_DELAYS = [3000, 10000, 30000]; // 3sec, 10sec, 30sec
  
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
    
    // Initialize translations on page
    updatePageTranslations();
    
    // Initialize language switcher
    setupLanguageSwitcher();
  }
  
  /**
   * Setup language switcher
   */
  function setupLanguageSwitcher() {
    // Find language selector
    const languageSelector = document.querySelector('.language-selector');
    if (!languageSelector) return;
    
    // Clear content
    languageSelector.innerHTML = '';
    
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
    
    // Create selector options
    languages.forEach(lang => {
      const option = document.createElement('div');
      option.className = 'language-option';
      option.dataset.lang = lang.code;
      option.innerHTML = `${lang.flag} ${lang.name}`;
      
      if (lang.code === currentLanguage) {
        option.classList.add('active');
      }
      
      option.addEventListener('click', function() {
        changeLanguage(lang.code);
      });
      
      languageSelector.appendChild(option);
    });
    
    // "More languages" button
    const moreBtn = document.createElement('div');
    moreBtn.className = 'language-more-btn';
    moreBtn.textContent = '...';
    moreBtn.addEventListener('click', function() {
      showAllLanguages(languageSelector);
    });
    
    languageSelector.appendChild(moreBtn);
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
      item.addEventListener('click', function() {
        changeLanguage(lang.code);
        document.body.removeChild(modal);
      });
      
      grid.appendChild(item);
    });
    
    // Close handler
    modal.querySelector('.language-modal-close').addEventListener('click', function() {
      document.body.removeChild(modal);
    });
    
    // Close on click outside modal
    modal.addEventListener('click', function(e) {
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
    
    // Update translations on page
    updatePageTranslations();
    
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
    document.dispatchEvent(new CustomEvent('languageChanged', { detail: { language: lang } }));
  }
  
  /**
   * Update translations on page
   */
  function updatePageTranslations() {
    // If language equals default language, no translation needed
    if (currentLanguage === defaultLanguage) return;
    
    // Reset retry counters on full page update
    translationRetryCount = {};
    
    // First collect all untranslated elements (with data-i18n attributes)
    const elements = document.querySelectorAll('[data-i18n]');
    
    // Process the elements in batches for better performance
    processElementsInBatches(elements, 0);
    
    // Also handle attribute translations
    const attributeElements = document.querySelectorAll('[data-i18n-attr]');
    if (attributeElements.length > 0) {
      processAttributeTranslations(attributeElements);
    }
  }
  
  /**
   * Process elements in batches to avoid UI blocking
   */
  function processElementsInBatches(elements, startIndex, batchSize = 50) {
    const endIndex = Math.min(startIndex + batchSize, elements.length);
    const batch = Array.from(elements).slice(startIndex, endIndex);
    
    // Collect texts to translate
    const textsToTranslate = [];
    const keysMap = [];
    
    batch.forEach(el => {
      const key = el.getAttribute('data-i18n');
      textsToTranslate.push(el.textContent.trim());
      keysMap.push(key);
    });
    
    // Request translations for this batch
    if (textsToTranslate.length > 0) {
      translateBatch(textsToTranslate, batch, keysMap);
    }
    
    // If there are more elements, schedule the next batch
    if (endIndex < elements.length) {
      setTimeout(() => {
        processElementsInBatches(elements, endIndex, batchSize);
      }, 0);
    }
  }
  
  /**
   * Translate a batch of elements
   */
  function translateBatch(texts, elements, keys) {
    fetch('/api/translate-batch', {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json'
      },
      body: JSON.stringify({
        texts: texts,
        targetLang: currentLanguage
      })
    })
    .then(response => response.json())
    .then(data => {
      if (data.translations && data.translations.length === texts.length) {
        // Apply translations
        elements.forEach((el, index) => {
          const key = keys[index];
          el.textContent = data.translations[index];
          
          // If translation was successful, we can remove the data-i18n attribute
          if (data.translations[index] !== texts[index] && 
              data.translations[index] !== key) {
            // Cache the translation for future use
            translationCache[`${currentLanguage}:${key}`] = data.translations[index];
            
            // Optionally remove the data-i18n attribute
            // el.removeAttribute('data-i18n');
          } else {
            // Translation failed or returned same text, schedule retry if needed
            scheduleRetryForElement(el, key);
          }
        });
      }
    })
    .catch(error => {
      console.error('Translation error:', error);
    });
  }
  
  /**
   * Process attribute translations
   */
  function processAttributeTranslations(elements) {
    const attributeTexts = [];
    const attributeMappings = [];
    
    elements.forEach(el => {
      try {
        const attrsMap = JSON.parse(el.getAttribute('data-i18n-attr'));
        for (const [attr, key] of Object.entries(attrsMap)) {
          attributeTexts.push(el.getAttribute(attr));
          attributeMappings.push({ 
            element: el, 
            attribute: attr,
            key: key
          });
        }
      } catch (e) {
        console.error('Error parsing data-i18n-attr:', e);
      }
    });
    
    // If there are attributes to translate
    if (attributeTexts.length > 0) {
      fetch('/api/translate-batch', {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json'
        },
        body: JSON.stringify({
          texts: attributeTexts,
          targetLang: currentLanguage
        })
      })
      .then(response => response.json())
      .then(data => {
        if (data.translations && data.translations.length === attributeTexts.length) {
          // Apply translations
          attributeMappings.forEach((mapping, index) => {
            mapping.element.setAttribute(mapping.attribute, data.translations[index]);
            
            // Cache the translation
            translationCache[`${currentLanguage}:${mapping.key}`] = data.translations[index];
            
            // Schedule retry if needed
            if (data.translations[index] === attributeTexts[index] || 
                data.translations[index] === mapping.key) {
              scheduleRetryForAttribute(mapping.element, mapping.attribute, mapping.key);
            }
          });
        }
      })
      .catch(error => {
        console.error('Attribute translation error:', error);
      });
    }
  }
  
  /**
   * Schedule retry for element translation
   */
  function scheduleRetryForElement(element, key) {
    const retryKey = `${currentLanguage}:${key}`;
    
    // Initialize retry count if not exists
    if (!translationRetryCount[retryKey]) {
      translationRetryCount[retryKey] = 0;
    }
    
    // Check if we haven't exceeded max retries
    if (translationRetryCount[retryKey] < MAX_RETRIES) {
      const retryIndex = translationRetryCount[retryKey];
      const delay = RETRY_DELAYS[retryIndex];
      
      setTimeout(() => {
        // Add loading indicator class
        element.classList.add('i18n-loading');
        
        // Retry translation
        fetch('/api/translate', {
          method: 'POST',
          headers: {
            'Content-Type': 'application/json'
          },
          body: JSON.stringify({
            text: element.textContent.trim(),
            targetLang: currentLanguage
          })
        })
        .then(response => response.json())
        .then(data => {
          // Remove loading indicator
          element.classList.remove('i18n-loading');
          
          if (data.translated && data.translated !== element.textContent) {
            element.textContent = data.translated;
            translationCache[retryKey] = data.translated;
            // Optionally remove data-i18n attribute
            // element.removeAttribute('data-i18n');
          }
        })
        .catch(error => {
          element.classList.remove('i18n-loading');
          console.error('Retry translation error:', error);
        });
        
        // Increment retry count
        translationRetryCount[retryKey]++;
      }, delay);
    }
  }
  
  /**
   * Schedule retry for attribute translation
   */
  function scheduleRetryForAttribute(element, attribute, key) {
    const retryKey = `${currentLanguage}:${key}:attr:${attribute}`;
    
    if (!translationRetryCount[retryKey]) {
      translationRetryCount[retryKey] = 0;
    }
    
    if (translationRetryCount[retryKey] < MAX_RETRIES) {
      const retryIndex = translationRetryCount[retryKey];
      const delay = RETRY_DELAYS[retryIndex];
      
      setTimeout(() => {
        // Add loading indicator to parent element
        element.classList.add('i18n-loading');
        
        fetch('/api/translate', {
          method: 'POST',
          headers: {
            'Content-Type': 'application/json'
          },
          body: JSON.stringify({
            text: element.getAttribute(attribute),
            targetLang: currentLanguage
          })
        })
        .then(response => response.json())
        .then(data => {
          element.classList.remove('i18n-loading');
          
          if (data.translated && data.translated !== element.getAttribute(attribute)) {
            element.setAttribute(attribute, data.translated);
            translationCache[retryKey] = data.translated;
          }
        })
        .catch(error => {
          element.classList.remove('i18n-loading');
          console.error('Retry attribute translation error:', error);
        });
        
        translationRetryCount[retryKey]++;
      }, delay);
    }
  }
  
  /**
   * Function to translate dynamically created element
   * @param {HTMLElement} element - HTML element to translate
   * @param {string} context - Translation context
   * @returns {Promise} - Promise with translation result
   */
  function translateDynamicElement(element, context = '') {
    // If language equals default, no translation needed
    if (currentLanguage === defaultLanguage) return Promise.resolve();
    
    // First, look for any data-i18n attributes
    const i18nElements = element.querySelectorAll('[data-i18n]');
    const i18nAttrElements = element.querySelectorAll('[data-i18n-attr]');
    
    if (i18nElements.length > 0) {
      processElementsInBatches(i18nElements, 0);
    }
    
    if (i18nAttrElements.length > 0) {
      processAttributeTranslations(i18nAttrElements);
    }
    
    // Now handle text nodes that don't have data-i18n already
    const textNodes = [];
    const walker = document.createTreeWalker(
      element,
      NodeFilter.SHOW_TEXT,
      null,
      false
    );
    
    let node;
    while (node = walker.nextNode()) {
      const parent = node.parentNode;
      
      // Skip if parent already has data-i18n attribute or is a script
      if (parent.nodeType === Node.ELEMENT_NODE && 
          (parent.hasAttribute('data-i18n') || 
           parent.tagName === 'SCRIPT' || 
           parent.tagName === 'STYLE')) {
        continue;
      }
      
      if (node.nodeValue.trim() !== '') {
        textNodes.push(node);
      }
    }
    
    // Attributes with text to translate
    const attributesToTranslate = ['placeholder', 'title', 'value', 'alt'];
    
    // Find elements with these attributes
    const elementsWithAttributes = element.querySelectorAll(
      attributesToTranslate.map(attr => `[${attr}]`).join(',')
    );
    
    // All text values to send to server
    const textsToTranslate = [];
    
    // Add text nodes
    textNodes.forEach(node => {
      textsToTranslate.push(node.nodeValue.trim());
    });
    
    // Add attribute values
    elementsWithAttributes.forEach(el => {
      attributesToTranslate.forEach(attr => {
        if (el.hasAttribute(attr) && el.getAttribute(attr).trim() !== '') {
          textsToTranslate.push(el.getAttribute(attr));
        }
      });
    });
    
    // If nothing to translate, exit
    if (textsToTranslate.length === 0) return Promise.resolve();
    
    // Send translation request
    return fetch('/api/translate-batch', {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json'
      },
      body: JSON.stringify({
        texts: textsToTranslate,
        targetLang: currentLanguage,
        context: context
      })
    })
    .then(response => response.json())
    .then(data => {
      if (!data.translations || data.translations.length !== textsToTranslate.length) {
        throw new Error('Invalid translation response');
      }
      
      // Apply translations to text nodes
      let index = 0;
      textNodes.forEach(node => {
        node.nodeValue = data.translations[index++];
      });
      
      // Apply translations to attributes
      elementsWithAttributes.forEach(el => {
        attributesToTranslate.forEach(attr => {
          if (el.hasAttribute(attr) && el.getAttribute(attr).trim() !== '') {
            el.setAttribute(attr, data.translations[index++]);
          }
        });
      });
      
      return data;
    })
    .catch(error => {
      console.error('Translation error:', error);
      return null;
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
  
  // Export functions to global namespace
  window.i18n = {
    init,
    changeLanguage,
    getCurrentLanguage: () => currentLanguage || defaultLanguage,
    updatePageTranslations,
    translateDynamicElement,
    isInitialized: () => currentLanguage !== null,
    updateTranslations: updatePageTranslations
  };
  
  // Call async initialization when script loads
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', () => init());
  } else {
    init();
  }
})();