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
  document.getElementById('euPopup').classList.remove('visible');
  document.getElementById('unPopup').classList.remove('visible');
  
  // Get current language
  const previousLanguage = getCurrentLanguage();
  
  // If selected same language, just close menu
  if (langCode === previousLanguage) {
    return;
  }
  
  // Update SVG masks (important visual feature from original)
  try {
    document.getElementById(`${previousLanguage}Mask`).setAttribute("mask", "url(#maskClose)");
  } catch (e) { /* Ignore errors */ }
  
  try {
    document.getElementById(`${langCode}Mask`).setAttribute("mask", "url(#maskOpen)");
  } catch (e) { /* Ignore errors */ }
  
  // Use i18n function if available
  if (window.i18n && typeof window.i18n.changeLanguage === 'function') {
    window.i18n.changeLanguage(langCode);
  } else {
    // Fallback to original implementation
    window.language = langCode;
    window.menuUsed = true;
    
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
 * Original language selection function from old implementation
 * @param {string} langPos - Language position or identifier
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
    newLanguage = document.getElementById(langPos).getAttribute("href").slice(1);
  } else {
    newLanguage = langPos;
  }
  
  if (newLanguage === previousLanguage) return;

  // Critical: Update SVG masks for visual feedback
  try {
    document.getElementById(`${previousLanguage}Mask`).setAttribute("mask", "url(#maskClose)");
  } catch (e) { /* Ignore errors */ }
  
  try {
    document.getElementById(`${newLanguage}Mask`).setAttribute("mask", "url(#maskOpen)");
  } catch (e) { /* Ignore errors */ }

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
 * Initialize language selector with outside click handling
 */
export function initLanguageSelector() {
  // Sync language masks on initialization
  syncLanguageMasks();
  
  // Add event listener for clicks outside language popup
  document.addEventListener('click', function(event) {
    const euPopup = document.getElementById('euPopup');
    const unPopup = document.getElementById('unPopup');
    
    // Check click for EU menu
    const euButton = document.querySelector('[onclick="setLanguage(\'lang2\')"]');
    if (euPopup && euPopup.classList.contains('visible') && 
        !euPopup.contains(event.target) && 
        (!euButton || !euButton.contains(event.target))) {
      euPopup.classList.remove('visible');
    }
    
    // Check click for UN menu
    const unButton = document.querySelector('[onclick="setLanguage(\'lang1\')"]');
    if (unPopup && unPopup.classList.contains('visible') && 
        !unPopup.contains(event.target) && 
        (!unButton || !unButton.contains(event.target))) {
      unPopup.classList.remove('visible');
    }
  });
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

// Initialize language selector when DOM is ready
document.addEventListener('DOMContentLoaded', initLanguageSelector);

// Export to window for inline handlers
window.toggleLanguagePopup = toggleLanguagePopup;
window.selectLanguage = selectLanguage; 
window.setLanguage = setLanguage;
window.syncLanguageMasks = syncLanguageMasks;