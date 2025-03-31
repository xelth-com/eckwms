/**
 * Language Selector Module
 * Handles language switching and flag masks
 */

/**
 * Shows or hides language selection popup
 * @param {string} popupType - Type of popup ('eu' or 'un')
 */
export function toggleLanguagePopup(popupType) {
  const euPopup = document.getElementById('euPopup');
  const unPopup = document.getElementById('unPopup');
  
  if (!euPopup || !unPopup) return;
  
  // Close all menus if they are open
  if (popupType === 'eu') {
    if (euPopup.classList.contains('visible')) {
      euPopup.classList.remove('visible');
    } else {
      unPopup.classList.remove('visible'); // Close other menu
      euPopup.classList.add('visible');
      highlightCurrentLanguage(euPopup);
    }
  } else if (popupType === 'un') {
    if (unPopup.classList.contains('visible')) {
      unPopup.classList.remove('visible');
    } else {
      euPopup.classList.remove('visible'); // Close other menu
      unPopup.classList.add('visible');
      highlightCurrentLanguage(unPopup);
    }
  }
}

/**
 * Highlights current selected language in popup
 * @param {HTMLElement} popup - Popup element
 */
function highlightCurrentLanguage(popup) {
  const currentLang = getCurrentLanguage();
  const buttons = popup.querySelectorAll('.langButton');
  
  buttons.forEach(button => {
    button.classList.remove('active');
    
    // Extract language code from click handler
    const onclickAttr = button.getAttribute('onclick');
    if (onclickAttr) {
      const langMatch = onclickAttr.match(/selectLanguage\(['"]([a-z]{2})['"]\)/);
      if (langMatch && langMatch[1] === currentLang) {
        button.classList.add('active');
      }
    }
    
    // Also check data-language attribute
    const dataLang = button.getAttribute('data-language');
    if (dataLang === currentLang) {
      button.classList.add('active');
    }
  });
}

/**
 * Gets current language from various sources
 * @returns {string} - Language code (e.g., 'en')
 */
export function getCurrentLanguage() {
  // Check global variable
  if (typeof window.language !== 'undefined' && window.language) {
    return window.language;
  }
  
  // Check HTML lang attribute
  const htmlLang = document.documentElement.lang;
  if (htmlLang) {
    return htmlLang;
  }
  
  // Check i18next cookie
  const cookieMatch = document.cookie.match(/i18next=([^;]+)/);
  if (cookieMatch) {
    return cookieMatch[1];
  }
  
  // Check localStorage
  try {
    const lsLang = localStorage.getItem('i18nextLng');
    if (lsLang) {
      return lsLang;
    }
  } catch (e) {
    // Ignore localStorage errors
  }
  
  // Default to English
  return 'en';
}

/**
 * Selects a language from popup
 * @param {string} langCode - Language code to select
 */
export function selectLanguage(langCode) {
  // Close all popups
  const euPopup = document.getElementById('euPopup');
  const unPopup = document.getElementById('unPopup');
  
  if (euPopup) euPopup.classList.remove('visible');
  if (unPopup) unPopup.classList.remove('visible');
  
  // Get current language
  const previousLanguage = getCurrentLanguage();
  
  // If selected same language, just close menu
  if (langCode === previousLanguage) {
    return;
  }
  
  // Set menuUsed flag for auto-display logic
  window.menuUsed = true;
  
  // Update SVG masks (important visual feature from original)
  updateLanguageMasks(previousLanguage, langCode);
  
  // Use i18n function if available
  if (window.i18n && typeof window.i18n.changeLanguage === 'function') {
    window.i18n.changeLanguage(langCode);
  } else {
    // Fallback to original implementation
    window.language = langCode;
    
    // Save in cookie and localStorage
    document.cookie = `i18next=${langCode}; path=/; max-age=${60 * 60 * 24 * 365}`;
    try {
      localStorage.setItem('i18nextLng', langCode);
    } catch (e) { /* Ignore errors */ }
    
    // Reload page with new language
    const cacheBuster = Date.now();
    const url = new URL(window.location.href);
    url.searchParams.set('i18n_cb', cacheBuster);
    url.searchParams.set('lang', langCode);
    window.location.href = url.toString();
  }
}

/**
 * Updates the language masks in SVGs
 * @param {string} previousLang - Previous language code
 * @param {string} newLang - New language code
 */
function updateLanguageMasks(previousLang, newLang) {
  try {
    const prevMask = document.getElementById(`${previousLang}Mask`);
    if (prevMask) {
      prevMask.setAttribute("mask", "url(#maskClose)");
    }
  } catch (e) { /* Ignore errors */ }
  
  try {
    const newMask = document.getElementById(`${newLang}Mask`);
    if (newMask) {
      newMask.setAttribute("mask", "url(#maskOpen)");
    }
  } catch (e) { /* Ignore errors */ }
}

/**
 * Legacy language selection function for compatibility
 * @param {string} langPos - Language position or code
 */
export function setLanguage(langPos) {
  // Handle special cases for popup menus
  if (langPos === 'lang2' || langPos === 'eu') {
    toggleLanguagePopup('eu');
    return;
  } else if (langPos === 'lang1' || langPos === 'un') {
    toggleLanguagePopup('un');
    return;
  }
  
  // Get previous language
  const previousLanguage = getCurrentLanguage();
  let newLanguage;
  
  // Extract language code
  if (langPos.slice(0, 4) === "lang") {
    window.menuUsed = true;
    const langElement = document.getElementById(langPos);
    if (langElement) {
      // Try different ways to extract language code
      const hrefLang = langElement.getAttribute("href");
      if (hrefLang && hrefLang.startsWith("#")) {
        newLanguage = hrefLang.slice(1);
      } else {
        // Look for SVG use element
        const useElement = langElement.querySelector("use");
        if (useElement) {
          const useHref = useElement.getAttribute("href");
          if (useHref && useHref.startsWith("#")) {
            newLanguage = useHref.slice(1);
          }
        }
      }
    }
  } else {
    newLanguage = langPos;
  }
  
  if (!newLanguage || newLanguage === previousLanguage) return;

  // Update masks for visual feedback
  updateLanguageMasks(previousLanguage, newLanguage);

  // Update language variable
  window.language = newLanguage;
  
  // Save preferences
  document.cookie = `i18next=${newLanguage}; path=/; max-age=${60 * 60 * 24 * 365}`;
  try {
    localStorage.setItem('i18nextLng', newLanguage);
  } catch (e) { /* Ignore errors */ }
  
  // Reload with new language
  const cacheBuster = Date.now();
  const url = new URL(window.location.href);
  url.searchParams.set('i18n_cb', cacheBuster);
  url.searchParams.set('lang', newLanguage);
  window.location.href = url.toString();
}

/**
 * Sync language masks with current language
 */
export function syncLanguageMasks() {
  try {
    const currentLang = getCurrentLanguage();
    console.log(`Synchronizing language masks for ${currentLang}`);
    
    // Close all masks
    const supportedLangs = ['de', 'en', 'fr', 'it', 'es', 'pt', 'nl', 'da', 'sv', 'fi', 
      'el', 'cs', 'pl', 'hu', 'sk', 'sl', 'et', 'lv', 'lt', 'ro',
      'bg', 'hr', 'ga', 'mt', 'ru', 'tr', 'ar', 'zh', 'uk', 'sr', 
      'he', 'ko', 'ja', 'no', 'bs', 'hi'];
    
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
    console.error("Error synchronizing language masks:", e);
  }
}

/**
 * Initialize language selector
 */
export function initLanguageSelector() {
  // Initial sync of language masks
  syncLanguageMasks();
  
  // Set up outside click handlers
  setupOutsideClickHandlers();
  
  // Update language buttons
  updateLanguageButtons();
}

/**
 * Set up handlers to close popup when clicking outside
 */
function setupOutsideClickHandlers() {
  document.addEventListener('click', function(event) {
    const euPopup = document.getElementById('euPopup');
    const unPopup = document.getElementById('unPopup');
    
    // EU popup outside click
    if (euPopup && euPopup.classList.contains('visible')) {
      const isClickInside = euPopup.contains(event.target);
      const isClickOnTrigger = event.target.closest('[data-language-popup="eu"]');
      
      if (!isClickInside && !isClickOnTrigger) {
        euPopup.classList.remove('visible');
      }
    }
    
    // UN popup outside click
    if (unPopup && unPopup.classList.contains('visible')) {
      const isClickInside = unPopup.contains(event.target);
      const isClickOnTrigger = event.target.closest('[data-language-popup="un"]');
      
      if (!isClickInside && !isClickOnTrigger) {
        unPopup.classList.remove('visible');
      }
    }
  });
}

/**
 * Update language buttons with proper event handlers
 */
function updateLanguageButtons() {
  // Set up popup triggers
  document.querySelectorAll('[data-language-popup]').forEach(btn => {
    btn.addEventListener('click', () => {
      const popupType = btn.getAttribute('data-language-popup');
      toggleLanguagePopup(popupType);
    });
  });
  
  // Set up language buttons in popup
  document.querySelectorAll('.langButton[data-language]').forEach(btn => {
    btn.addEventListener('click', () => {
      const langCode = btn.getAttribute('data-language');
      if (langCode) {
        selectLanguage(langCode);
      }
    });
  });
  
  // Set up language buttons in main menu
  document.querySelectorAll('#langMenu [data-language]').forEach(btn => {
    btn.addEventListener('click', () => {
      const langCode = btn.getAttribute('data-language');
      if (langCode) {
        setLanguage(langCode);
      }
    });
  });
  
  // Handle legacy onclick attributes
  document.querySelectorAll('[onclick^="setLanguage"]').forEach(btn => {
    const clickHandler = btn.getAttribute('onclick');
    if (clickHandler) {
      // Keep the original handler but ensure our code runs too
      btn.addEventListener('click', (e) => {
        // Let the original handler run first
        setTimeout(() => {
          // Check if we need to add our own handling
          if (window.setLanguage && !e.defaultPrevented) {
            const match = clickHandler.match(/setLanguage\(['"]([^'"]+)['"]\)/);
            if (match) {
              const param = match[1];
              if (param) setLanguage(param);
            }
          }
        }, 0);
      });
    }
  });
}

// Initialize when DOM is ready
if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', initLanguageSelector);
} else {
  initLanguageSelector();
}

// Export for global access
window.toggleLanguagePopup = toggleLanguagePopup;
window.selectLanguage = selectLanguage; 
window.setLanguage = setLanguage;
window.syncLanguageMasks = syncLanguageMasks;
window.getCurrentLanguage = getCurrentLanguage;