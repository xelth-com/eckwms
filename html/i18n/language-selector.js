/**
 * Language Selector Module
 * Handles language switching and flag masks
 */

// Track current language group (Group 1 or Group 2)
let currentLanguageGroup = 1;

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
 * Toggle between language groups (Group 1 and Group 2)
 */
export function toggleLanguageGroup() {
  console.log("Language group toggle initiated");
  
  const group1 = document.getElementById('langGroup1');
  const group2 = document.getElementById('langGroup2');
  
  if (!group1 || !group2) {
    console.error("Language groups not found in DOM");
    return;
  }
  
  if (currentLanguageGroup === 1) {
    // Switch to group 2
    group1.style.display = 'none';
    group2.style.display = 'flex';
    currentLanguageGroup = 2;
    console.log("Switched to language group 2");
  } else {
    // Switch to group 1
    group1.style.display = 'flex';
    group2.style.display = 'none';
    currentLanguageGroup = 1;
    console.log("Switched to language group 1");
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
 * @param {boolean} shouldToggleGroup - Whether to toggle groups if current language is in a different group
 */
export function syncLanguageMasks(shouldToggleGroup = true) {
  try {
    const currentLang = getCurrentLanguage();
    console.log(`Synchronizing language masks for ${currentLang}`);
    
    // Close all masks
    const supportedLangs = [
      // --- EU Languages ---
      'en', 'de', 'pl', 'fr', 'it', 'es', 'nl', 'ro', 'hr', 'bg', 'el',
      'pt', 'cs', 'hu', 'sv', 'da', 'fi', 'sk', 'lt', 'lv', 'et', 'sl',
      'mt', 'ga',
      // --- Non-EU Languages ---
      'tr', 'ru', 'ar', 'uk', 'sr', 'bs', 'no', 'zh', 'hi', 'ja', 'ko',
      'he'
    ];
    
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
      
      // Only attempt to toggle group if explicitly requested
      if (shouldToggleGroup) {
        const group1 = document.getElementById('langGroup1');
        const group2 = document.getElementById('langGroup2');
        
        if (group1 && group2) {
          try {
            // Check if current language is in group 2
            const group2Languages = Array.from(document.querySelectorAll('#langGroup2 [data-language]'))
              .map(el => el.getAttribute('data-language'));
            
            // Check if current language is in group 1
            const group1Languages = Array.from(document.querySelectorAll('#langGroup1 [data-language]'))
              .map(el => el.getAttribute('data-language'));
            
            // Update display based on which group contains the current language
            if (group2Languages.includes(currentLang) && currentLanguageGroup === 1) {
              // Current language is in group 2 but group 1 is shown
              group1.style.display = 'none';
              group2.style.display = 'flex';
              currentLanguageGroup = 2;
              console.log('Switched to language group 2');
            } else if (group1Languages.includes(currentLang) && currentLanguageGroup === 2) {
              // Current language is in group 1 but group 2 is shown
              group1.style.display = 'flex';
              group2.style.display = 'none';
              currentLanguageGroup = 1;
              console.log('Switched to language group 1');
            }
          } catch (innerError) {
            console.error("Error in group switching logic:", innerError);
          }
        }
      }
    }
  } catch (e) {
    console.error("Error synchronizing language masks:", e);
  }
}

/**
 * Initialize language selector
 */
export function initLanguageSelector() {
  console.log("Initializing language selector");
  
  // Set default language group
  currentLanguageGroup = 1;
  
  // Initial sync of language masks
  syncLanguageMasks();
  
  // Set up outside click handlers for popups
  setupOutsideClickHandlers();
  
  // Update language buttons
  updateLanguageButtons();
  
  // Apply CSS fixes for language toggle button
  applyLanguageToggleCSS();
  
  // Make sure only one group is visible initially
  ensureOneGroupVisible();
  
  console.log("Language selector initialized");
}

/**
 * Ensure only one language group is visible
 */
function ensureOneGroupVisible() {
  const group1 = document.getElementById('langGroup1');
  const group2 = document.getElementById('langGroup2');
  
  if (group1 && group2) {
    // Default to group 1 visible, group 2 hidden
    group1.style.display = 'flex';
    group2.style.display = 'none';
    currentLanguageGroup = 1;
    
    // Check if current language is in group 2
    const currentLang = getCurrentLanguage();
    const group2Languages = Array.from(document.querySelectorAll('#langGroup2 [data-language]'))
      .map(el => el.getAttribute('data-language'));
    
    if (group2Languages.includes(currentLang)) {
      // If current language is in group 2, switch to it
      group1.style.display = 'none';
      group2.style.display = 'flex';
      currentLanguageGroup = 2;
    }
  }
}

/**
 * Apply CSS fixes for language toggle button
 */
function applyLanguageToggleCSS() {
  // Create style element for fixes
  const styleElement = document.createElement('style');
  styleElement.textContent = `
    /* Fix language menu layout */
    #langMenu {
      display: flex !important;
      flex-wrap: nowrap !important;
      align-items: center !important;
      margin-left: auto;
      padding: 3px;
      flex-direction: row-reverse !important; /* RTL layout */
    }
    
    .langButtonGroup {
      display: flex !important;
      flex-wrap: nowrap !important;
      flex-direction: row-reverse !important; /* RTL layout */
    }
    
    #langToggleBtn {
      display: inline-block !important;
      margin: 0 3px;
      vertical-align: middle;
      order: 1 !important; /* Put toggle button on the left */
    }
    
    #langMenu .button {
      margin: 0 2px;
      vertical-align: middle;
    }

    /* Ensure only one group is visible initially */
    #langGroup2 {
      display: none;
    }
  `;
  document.head.appendChild(styleElement);
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
  // Set up language toggle button
  const toggleBtn = document.getElementById('langToggleBtn');
  if (toggleBtn) {
    toggleBtn.removeEventListener('click', toggleLanguageGroup); // Remove any existing handlers
    toggleBtn.addEventListener('click', toggleLanguageGroup);
    console.log('Added event listener to language toggle button');
  } else {
    console.warn('Language toggle button not found in DOM');
  }
  
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
  // If DOM is already loaded, run now
  setTimeout(initLanguageSelector, 0);
}

// Export for global access
window.toggleLanguagePopup = toggleLanguagePopup;
window.selectLanguage = selectLanguage; 
window.setLanguage = setLanguage;
window.syncLanguageMasks = syncLanguageMasks;
window.getCurrentLanguage = getCurrentLanguage;
window.toggleLanguageGroup = toggleLanguageGroup;