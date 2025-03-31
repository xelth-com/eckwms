/**
 * Side Menu Module
 * Mobile-friendly navigation menu that displays on smaller screens
 */

import { loadCSS, loadTemplate } from '/core/module-loader.js';

// Track transition state globally
window.waitForTransition = false;

/**
 * Initialize side menu
 * @param {HTMLElement} container - Container to render into
 */
export async function init(container) {
  // Load required CSS
  await loadCSS('/navigation/side-menu.css');
  
  // Load HTML template
  const html = await loadTemplate('/navigation/side-menu.template.html');
  container.innerHTML = html;
  
  // Initialize event listeners
  initEventListeners();
  
  // Apply SVG backgrounds to buttons
  applyButtonBackgrounds();
}

/**
 * Initialize side menu event listeners
 */
function initEventListeners() {
  const menuToggle = document.getElementById('sideMenuToggle');
  if (menuToggle) {
    menuToggle.removeEventListener('click', sideMenuToggleHandler);
    menuToggle.addEventListener('click', sideMenuToggleHandler);
  }
  
  // Add language selection handlers
  document.querySelectorAll('[data-language]').forEach(button => {
    button.addEventListener('click', () => {
      if (window.setLanguage) {
        const lang = button.getAttribute('data-language');
        window.setLanguage(lang);
      }
    });
  });
}

/**
 * Side menu toggle click handler
 */
function sideMenuToggleHandler() {
  if (window.showMenu) {
    window.showMenu('sideMenu');
  }
}

/**
 * Apply SVG backgrounds to buttons
 */
function applyButtonBackgrounds() {
  if (!window.backSvg2) return;
  
  const backButtonImg = `url(data:image/svg+xml;charset=utf-8;base64,${btoa(window.backSvg2)})`;
  
  Array.from(document.querySelectorAll('.sideMenu.button')).forEach(element => {
    element.style.backgroundImage = backButtonImg;
  });
}

/**
 * Toggle side menu visibility
 * @param {string} menuType - Menu type identifier ('sideMenu')
 */
export function showMenu(menuType) {
  if (window.waitForTransition) return;
  const elements = Array.from(document.getElementsByClassName(`${menuType}Line`));
  const buttonsElement = document.getElementById(`${menuType}Buttons`);

  if (!elements.length || !buttonsElement) return;

  if (buttonsElement.style.display !== "none") {
    window.waitForTransition = true;
    setTimeout(() => {
      buttonsElement.style.display = "none";
      window.waitForTransition = false;
    }, 3000);
  } else {
    buttonsElement.style.display = "inline-block";
  }

  if (elements[1].getAttribute("x") === "10") {
    // Open menu animation
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
    // Close menu animation
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
 * Post-initialization tasks
 */
export function postInit() {
  // Apply SVG backgrounds after DOM updates
  applyButtonBackgrounds();
}

// Expose functions for global access
window.showSideMenu = showMenu;