/**
 * Footer Module
 * Handles the site footer with copyright and links
 */

import { loadCSS, loadTemplate } from '/modules/core/module-loader.js';

/**
 * Initialize footer content
 * @param {HTMLElement} container - Container to render footer into
 */
export async function init(container) {
  // Load required CSS
  await loadCSS('/modules/footer/footer.css');
  
  // Load HTML template
  const html = await loadTemplate('/modules/footer/footer.template.html');
  container.innerHTML = html;
  
  // Set current year for copyright
  updateCopyrightYear();
}

/**
 * Update copyright year to current year
 */
function updateCopyrightYear() {
  const yearElement = document.getElementById('copyright-year');
  if (yearElement) {
    yearElement.textContent = new Date().getFullYear();
  }
}

/**
 * Post-initialization tasks
 */
export function postInit() {
  // Any post-initialization tasks
}
