/**
 * Header Module
 * Contains the site logo, language selector, and main navigation menu
 */

import { loadCSS, loadTemplate } from '/modules/core/module-loader.js';
import { toggleLanguagePopup, selectLanguage } from '/modules/i18n/language-selector.js';

/**
 * Header module initialization
 * @param {HTMLElement} container - Container element to render the header into
 */
export async function init(container) {
  // Load required CSS
  await loadCSS('/modules/header/header.css');
  
  // Load HTML template
  const html = await loadTemplate('/modules/header/header.template.html');
  container.innerHTML = html;
  
  // Initialize event listeners
  initEventListeners();
}

/**
 * Initialize header-specific event listeners
 */
function initEventListeners() {
  // Main menu toggle
  const menuButton = document.getElementById('mainMenuToggle');
  if (menuButton) {
    menuButton.addEventListener('click', () => showMenu('mainMenu'));
  }
  
  // Language menu buttons
  const languageButtons = document.querySelectorAll('[data-language]');
  languageButtons.forEach(button => {
    const lang = button.getAttribute('data-language');
    button.addEventListener('click', () => selectLanguage(lang));
  });
  
  // Language popup controls
  const euButton = document.querySelector('[data-language-popup="eu"]');
  if (euButton) {
    euButton.addEventListener('click', () => toggleLanguagePopup('eu'));
  }
  
  const unButton = document.querySelector('[data-language-popup="un"]');
  if (unButton) {
    unButton.addEventListener('click', () => toggleLanguagePopup('un'));
  }
  
  // Close buttons for language popups
  const closeButtons = document.querySelectorAll('.langPopupClose');
  closeButtons.forEach(button => {
    button.addEventListener('click', function() {
      const popup = this.closest('.langPopup');
      if (popup) {
        popup.classList.remove('visible');
      }
    });
  });
}

/**
 * Show or hide a menu
 * @param {string} menuType - Type of menu to toggle ('mainMenu' or 'sideMenu')
 */
function showMenu(menuType) {
  if (window.waitForTransition) return;
  const elements = Array.from(document.getElementsByClassName(`${menuType}Line`));

  if (document.getElementById(`${menuType}Buttons`).style.display !== "none") {
    window.waitForTransition = true;
    setTimeout(() => {
      document.getElementById(`${menuType}Buttons`).style.display = "none";
      if (menuType === "mainMenu") document.getElementById("langMenu").style.display = "inline-block";
      window.waitForTransition = false;
    }, 3000);
  } else {
    if (menuType === "mainMenu") document.getElementById("langMenu").style.display = "none";
    document.getElementById(`${menuType}Buttons`).style.display = "inline-block";
  }

  if (elements[1].getAttribute("x") === "10") {
    elements[1].setAttribute("x", "65");
    elements[0].style.transform = "rotate(-45deg)";
    elements[2].style.transform = "rotate(45deg)";
    Array.from(document.getElementsByClassName(menuType)).forEach(element => {
      element.style.transitionDuration = `${(0.5 + Math.random())}s`;
      element.style.transitionDelay = `${Math.random()}s`;

      setTimeout(() => {
        element.style.visibility = "visible";
        element.style.opacity = 1;
      }, 67);
    });
    setTimeout(() => {
      Array.from(document.getElementsByClassName(menuType)).forEach(element => {
        element.style.transitionDuration = "0.3s";
        element.style.transitionDelay = "0s";
      });
    }, 2000);
  } else {
    elements[1].setAttribute("x", "10");
    elements[0].style.transform = "rotate(0deg)";
    elements[2].style.transform = "rotate(0deg)";
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
 * Post-initialization tasks, run after the DOM is updated
 */
export function postInit() {
  // Auto-show main menu on desktop after 30 seconds if not used yet
  setTimeout(() => {
    if (window.matchMedia("(min-width: 1001px)").matches) {
      if (document.getElementById("mainMenuButtons").style.display === "none") {
        if (!window.menuUsed) {
          showMenu("mainMenu");
        }
      }
    }
  }, 30000);
}

// Expose functions globally that need to be accessed by inline handlers
window.showMenu = showMenu;
