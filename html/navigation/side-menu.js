/**
 * Side Menu Module
 * Mobile-friendly navigation menu that displays on smaller screens
 */

import { loadCSS, loadTemplate } from '/core/module-loader.js';

/**
 * Initialize side menu
 * @param {HTMLElement} container - Container to render into
 */
export async function init(container) {
  // Only load and render on smaller screens
  if (window.innerWidth > 1100) {
    return;
  }
  
  // Load required CSS
  await loadCSS('/navigation/side-menu.css');
  
  // Load HTML template
  const html = await loadTemplate('/navigation/side-menu.template.html');
  container.innerHTML = html;
  
  // Initialize event listeners
  initEventListeners();
}

/**
 * Initialize side menu event listeners
 */
function initEventListeners() {
  const menuToggle = document.getElementById('sideMenuToggle');
  if (menuToggle) {
    menuToggle.addEventListener('click', () => showMenu('sideMenu'));
  }
}

/**
 * Toggle side menu visibility
 * @param {string} menuType - Menu type identifier
 */
function showMenu(menuType) {
  if (window.waitForTransition) return;
  const elements = Array.from(document.getElementsByClassName(`${menuType}Line`));

  if (document.getElementById(`${menuType}Buttons`).style.display !== "none") {
    window.waitForTransition = true;
    setTimeout(() => {
      document.getElementById(`${menuType}Buttons`).style.display = "none";
      window.waitForTransition = false;
    }, 3000);
  } else {
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
 * Post-initialization tasks
 */
export function postInit() {
  // Anything that needs to happen after DOM updates
}

// Expose showMenu for inline handlers
window.showSideMenu = showMenu;
