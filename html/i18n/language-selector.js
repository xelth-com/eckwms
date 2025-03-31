/**
 * Language Selector Module
 * Handles switching between languages and displaying language selection UI
 */

/**
 * Shows or hides the language selection popup
 * @param {string} popupType - Type of language popup ('eu' or 'un')
 */
export function toggleLanguagePopup(popupType) {
  const euPopup = document.getElementById('euPopup');
  const unPopup = document.getElementById('unPopup');
  
  // Close all popups if they're open
  if (popupType === 'eu') {
    if (euPopup.classList.contains('visible')) {
      euPopup.classList.remove('visible');
    } else {
      unPopup.classList.remove('visible'); // Close other popup
      euPopup.classList.add('visible');
      highlightCurrentLanguage(euPopup);
    }
  } else if (popupType === 'un') {
    if (unPopup.classList.contains('visible')) {
      unPopup.classList.remove('visible');
    } else {
      euPopup.classList.remove('visible'); // Close other popup
      unPopup.classList.add('visible');
      highlightCurrentLanguage(unPopup);
    }
  }
}

/**
 * Highlights the currently selected language in the popup
 * @param {HTMLElement} popup - The popup element containing language buttons
 */
function highlightCurrentLanguage(popup) {
  const currentLang = getCurrentLanguage();
  const buttons = popup.querySelectorAll('.langButton');
  
  buttons.forEach(button => {
    button.classList.remove('active');
    
    // Get language code from data attribute
    const langCode = button.getAttribute('data-language');
    if (langCode === currentLang) {
      button.classList.add('active');
    }
  });
}

/**
 * Gets the current language from various sources
 * @returns {string} - The language code (e.g., 'en', 'de')
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
 * Selects a language and updates UI
 * @param {string} langCode - Language code to switch to (e.g., 'en', 'de')
 */
export function selectLanguage(langCode) {
  // Close all popups
  document.getElementById('euPopup').classList.remove('visible');
  document.getElementById('unPopup').classList.remove('visible');
  
  // Get current language
  const previousLanguage = getCurrentLanguage();
  
  // If same language selected, just close popups
  if (langCode === previousLanguage) {
    return;
  }
  
  // Use i18n's changeLanguage if available
  if (window.i18n && window.i18n.changeLanguage) {
    window.i18n.changeLanguage(langCode);
  } else {
    // Fallback: traditional method
    reorganizeLanguageButtons(langCode, previousLanguage);
    
    // Update SVG masks as in original function
    try {
      document.getElementById(`${previousLanguage}Mask`).setAttribute("mask", "url(#maskClose)");
    } catch (e) { /* Ignore errors */ }
    
    try {
      document.getElementById(`${langCode}Mask`).setAttribute("mask", "url(#maskOpen)");
    } catch (e) { /* Ignore errors */ }
    
    // Update language variable
    window.language = langCode;
    window.menuUsed = true;
    
    // Save to cookie and localStorage
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
 * Reorganizes language buttons to optimize the display of the selected language
 * @param {string} selectedLang - Selected language code
 * @param {string} previousLang - Previous language code
 */
function reorganizeLanguageButtons(selectedLang, previousLang) {
  // Language menu container
  const langMenu = document.getElementById('langMenu');
  if (!langMenu) return;
  
  // Find button for selected language
  let selectedButton = null;
  const buttons = langMenu.querySelectorAll('.button');
  
  buttons.forEach(button => {
    const langCode = button.getAttribute('data-language');
    if (langCode === selectedLang) {
      selectedButton = button;
    }
  });
  
  // Find button for previous language
  let previousButton = null;
  buttons.forEach(button => {
    const langCode = button.getAttribute('data-language');
    if (langCode === previousLang) {
      previousButton = button;
    }
  });
  
  // If both buttons found, swap their positions
  if (selectedButton && previousButton) {
    const parent = langMenu;
    
    // Get original positions
    const prevPosition = Array.from(parent.children).indexOf(previousButton);
    const selectedPosition = Array.from(parent.children).indexOf(selectedButton);
    
    // Swap buttons
    if (prevPosition !== -1 && selectedPosition !== -1) {
      if (prevPosition < selectedPosition) {
        parent.insertBefore(selectedButton, previousButton);
      } else {
        parent.insertBefore(selectedButton, previousButton.nextSibling);
      }
    }
  }
}

/**
 * Initialize event handlers for clicks outside the language popups
 */
export function initLanguageSelector() {
  document.addEventListener('click', function(event) {
    const euPopup = document.getElementById('euPopup');
    const unPopup = document.getElementById('unPopup');
    
    // Check click for EU menu
    const euButton = document.querySelector('[data-language-popup="eu"]');
    if (euPopup && euPopup.classList.contains('visible') && 
        !euPopup.contains(event.target) && 
        (!euButton || !euButton.contains(event.target))) {
      euPopup.classList.remove('visible');
    }
    
    // Check click for UN menu
    const unButton = document.querySelector('[data-language-popup="un"]');
    if (unPopup && unPopup.classList.contains('visible') && 
        !unPopup.contains(event.target) && 
        (!unButton || !unButton.contains(event.target))) {
      unPopup.classList.remove('visible');
    }
  });
}

// Initialize when the module is loaded
document.addEventListener('DOMContentLoaded', initLanguageSelector);

// Expose functions globally that need to be accessed by inline handlers
window.toggleLanguagePopup = toggleLanguagePopup;
window.selectLanguage = selectLanguage;
