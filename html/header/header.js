/**
 * Header Module
 * Contains site logo, language selector, and main navigation menu
 */

import { loadCSS, loadTemplate } from '/core/module-loader.js';
import { syncLanguageMasks, initLanguageSelector } from '/i18n/language-selector.js';

// Globals for menu state tracking
window.waitForTransition = false;
window.menuUsed = false;
let cards = [];

/**
 * Initialize header module
 * @param {HTMLElement} container - Container to render header into
 */
export async function init(container) {
  // Load required CSS
  await loadCSS('/header/header.css');
  
  // Load HTML template
  const html = await loadTemplate('/header/header.template.html');
  container.innerHTML = html;
  
  // Initialize components
  initEventListeners();
  initMainMenuCards();
  applyButtonBackgrounds();
  
  // Ensure only one language group is visible initially
  ensureOneLanguageGroupVisible();
}

/**
 * Ensure only one language group is visible
 */
function ensureOneLanguageGroupVisible() {
  const group1 = document.getElementById('langGroup1');
  const group2 = document.getElementById('langGroup2');
  
  if (group1 && group2) {
    // Default to group 1 visible, group 2 hidden
    group1.style.display = 'flex';
    group2.style.display = 'none';
  }
}

/**
 * Initialize event listeners
 */
function initEventListeners() {
  // Add main menu toggle event
  const menuToggle = document.querySelector('#mainMenuToggle');
  if (menuToggle) {
    menuToggle.addEventListener('click', () => showMenu('mainMenu'));
  }
  
  // Add menu card hover events
  document.querySelectorAll('[onmouseenter^="mainMenuCardOpen"]').forEach(element => {
    const menuId = element.id;
    element.removeAttribute('onmouseenter');
    element.removeAttribute('onmouseleave');
    element.addEventListener('mouseenter', () => mainMenuCardOpen(menuId));
    element.addEventListener('mouseleave', () => mainMenuCardClose(menuId));
  });
  
  // Add click handlers for menu items
  document.querySelectorAll('.mainMenu[onclick]').forEach(element => {
    const onclickAttr = element.getAttribute('onclick');
    if (onclickAttr) {
      element.removeAttribute('onclick');
      element.addEventListener('click', (e) => {
        // Extract function and parameters
        const match = onclickAttr.match(/myFetch\(['"](.*?)['"],\s*['"](.*?)['"]\)/);
        if (match && window.myFetch) {
          const param1 = match[1];
          const param2 = match[2];
          window.myFetch(param1, param2);
        }
      });
    }
  });
  
  // Add language toggle button event listener
  const langToggleBtn = document.getElementById('langToggleBtn');
  if (langToggleBtn && window.toggleLanguageGroup) {
    langToggleBtn.addEventListener('click', function(e) {
      e.preventDefault();
      e.stopPropagation();
      window.toggleLanguageGroup();
    });
    console.log('Added event listener to language toggle button from header.js');
  }
  
  // Add language selection events
  document.querySelectorAll('#langMenu [data-language]').forEach(button => {
    button.addEventListener('click', function() {
      const langCode = this.getAttribute('data-language');
      if (langCode && window.setLanguage) {
        window.setLanguage(langCode);
      }
    });
  });
}

/**
 * Apply SVG backgrounds to buttons
 */
function applyButtonBackgrounds() {
  if (!window.backSvg2) return;
  
  const backButtonImg = `url(data:image/svg+xml;charset=utf-8;base64,${btoa(window.backSvg2)})`;
  window.backButtonImg = backButtonImg;
  
  // Apply to all buttons
  Array.from(document.getElementsByClassName("button")).forEach(element => {
    element.style.backgroundImage = backButtonImg;
  });
}

/**
 * Initialize main menu cards
 */
function initMainMenuCards() {
  cards = Array.from(document.getElementsByClassName("mainMenuCard"), (element, index) => {
    element.style.backgroundImage = `
      linear-gradient(90deg,#ba80 0%,#ba84 10%,#ba88 20%,#ba8c 30%,#ba8f 40%,  #ba8f 60%,#ba8c 70%,#ba88 80%,#ba84 90%,#ba80 100%),
      linear-gradient(30deg,#ba80,#ba80,#ba80,#ba8f,#ba8f,#ba80,#ba80,#ba80,#ba8f,#ba8f,#ba80,#ba80,#ba80,#ba8f,#ba8f,#ba80,#ba80,#ba80)`;
    
    // Add background image if available
    if (window.backButtonImg) {
      element.style.backgroundImage += `, ${window.backButtonImg}`;
    }
    
    return { 
      el: element, 
      mmn: "", 
      timeoutId: null, 
      timeoutId1: null 
    };
  });
}

/**
 * Toggle main menu visibility
 * @param {string} menuType - Type of menu ('mainMenu' or 'sideMenu')
 */
export function showMenu(menuType) {
  // Prevent multiple simultaneous transitions
  if (window.waitForTransition) return;
  
  // Find menu line elements and buttons container
  const elements = Array.from(document.getElementsByClassName(`${menuType}Line`));
  const buttonsElement = document.getElementById(`${menuType}Buttons`);
  
  // Exit if no elements found
  if (!elements.length || !buttonsElement) return;

  // Find language menu
  const langMenu = document.getElementById("langMenu");

  // Toggle menu visibility
  if (buttonsElement.style.display !== "none") {
    // Closing menu
    window.waitForTransition = true;
    setTimeout(() => {
      buttonsElement.style.display = "none";
      
      // Special handling for main menu to show language menu
      if (menuType === "mainMenu" && langMenu) {
        langMenu.style.display = "inline-block";
      }
      
      window.waitForTransition = false;
    }, 3000);
  } else {
    // Opening menu
    if (menuType === "mainMenu" && langMenu) {
      langMenu.style.display = "none";
    }
    buttonsElement.style.display = "inline-block";
  }

  // Animate menu lines
  if (elements.length > 1 && elements[1].getAttribute("x") === "10") {
    // Open menu animation
    elements[1].setAttribute("x", "65");
    elements[0].style.transform = "rotate(-45deg)";
    elements[2].style.transform = "rotate(45deg)";
    
    // Animate menu items
    Array.from(document.getElementsByClassName(menuType)).forEach(element => {
      element.style.transitionDuration = `${(0.5 + Math.random())}s`;
      element.style.transitionDelay = `${Math.random()}s`;

      setTimeout(() => {
        element.style.visibility = "visible";
        element.style.opacity = 1;
      }, 67);
    });
    
    // Reset transition timing
    setTimeout(() => {
      Array.from(document.getElementsByClassName(menuType)).forEach(element => {
        element.style.transitionDuration = "0.3s";
        element.style.transitionDelay = "0s";
      });
    }, 2000);
  } else if (elements.length > 1) {
    // Close menu animation
    elements[1].setAttribute("x", "10");
    elements[0].style.transform = "rotate(0deg)";
    elements[2].style.transform = "rotate(0deg)";
    
    // Animate menu items out
    Array.from(document.getElementsByClassName(menuType)).forEach(element => {
      element.style.transitionDuration = `${(0.5 + Math.random())}s`;
      element.style.transitionDelay = `${Math.random()}s`;
      
      setTimeout(() => {
        element.style.visibility = "hidden";
        element.style.opacity = 0;
      }, 69);
    });
  }
}

/**
 * Open main menu card
 * @param {string} mainMenuNumber - Main menu ID
 */
export function mainMenuCardOpen(mainMenuNumber) {
  const menu = document.getElementById(mainMenuNumber);
  if (!menu) return;
  
  menu.style.backgroundColor = "#ba87";
  
  // Find minimum and maximum z-index
  let zmin = parseInt(cards[0]?.el?.style?.zIndex) || 0;
  let zmax = parseInt(cards[0]?.el?.style?.zIndex) || 0;
  let equal = false;
  let i = 0;
  
  // Check if card is already open for this menu
  cards.forEach((element, index) => {
    if (element.mmn === mainMenuNumber) {
      equal = true;
      clearTimeout(element.timeoutId);
      clearTimeout(element.timeoutId1);
    }
    
    const z = parseInt(element.el.style.zIndex) || 0;
    if (zmin >= z) {
      zmin = z;
      i = index;
    }
    if (zmax < z) {
      zmax = z;
    }
  });
  
  // If already showing this menu, return
  if (equal) {
    return;
  } else {
    // Hide other cards
    cards.forEach((element, index) => {
      if (index !== i) {
        element.el.style.opacity = "0";
        element.el.style.filter = "blur(10px)";
        element.mmn = "empty";
        element.el.onmouseenter = null;
        element.el.onmouseleave = null;
      }
    });
  }

  // Clear timeouts and show card
  clearTimeout(cards[i].timeoutId);
  clearTimeout(cards[i].timeoutId1);
  
  cards[i].el.style.zIndex = `${zmax + 1}`;
  cards[i].el.style.display = "block";
  cards[i].el.onmouseenter = () => mainMenuCardOpen(mainMenuNumber);
  cards[i].el.onmouseleave = () => mainMenuCardClose(mainMenuNumber);
  
  // Get content from hidden div 
  const hiddenDiv = menu.querySelector('div[hidden]');
  if (hiddenDiv) {
    cards[i].el.innerHTML = hiddenDiv.innerHTML;
  }
  
  cards[i].mmn = mainMenuNumber;
  
  // Position card
  const event = window.event;
  if (event) {
    cards[i].el.style.left = `${parseInt(event.clientX - (event.clientX * cards[i].el.offsetWidth / window.innerWidth))}px`;
    cards[i].el.style.top = `${parseInt(Math.random() * 50 + 70)}px`;
  }
  
  cards[i].el.style.opacity = "1";
  cards[i].el.style.filter = "blur(0px)";
}

/**
 * Close main menu card
 * @param {string} mainMenuNumber - Main menu ID
 */
export function mainMenuCardClose(mainMenuNumber) {
  const menu = document.getElementById(mainMenuNumber);
  if (!menu) return;
  
  menu.style.backgroundColor = "#ba80";

  cards.forEach((element) => {
    if (element.mmn === mainMenuNumber) {
      element.timeoutId = setTimeout(() => {
        element.el.style.opacity = "0";
        element.el.style.filter = "blur(10px)";
        element.mmn = "empty";
        element.el.onmouseenter = null;
        element.el.onmouseleave = null;
        
        element.timeoutId1 = setTimeout(() => {
          element.el.style.display = "none";
        }, 500);
      }, 1000);
    }
  });
}

/**
 * Fix language toggle button CSS
 */
function fixLanguageToggleCSS() {
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
    #langGroup1 {
      display: flex !important;
    }
    
    #langGroup2 {
      display: none !important;
    }
  `;
  document.head.appendChild(styleElement);
}

/**
 * Post-initialization tasks
 */
export function postInit() {
  // Fix language toggle button CSS
  fixLanguageToggleCSS();
  
  // Initialize language selector
  if (typeof initLanguageSelector === 'function') {
    initLanguageSelector();
  } else if (window.syncLanguageMasks) {
    window.syncLanguageMasks();
  }
  
  // Auto-show main menu on desktop after delay if not used
  setTimeout(() => {
    if (window.matchMedia("(min-width: 1001px)").matches) {
      const menuButtons = document.getElementById("mainMenuButtons");
      if (menuButtons && menuButtons.style.display === "none") {
        if (!window.menuUsed) {
          showMenu("mainMenu");
        }
      }
    }
  }, 30000);
}

// Export functions for global access
window.showMenu = showMenu;
window.mainMenuCardOpen = mainMenuCardOpen;
window.mainMenuCardClose = mainMenuCardClose;