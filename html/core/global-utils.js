/**
 * Global Utilities
 * Common utility functions used across the site
 */

/**
 * Verify JWT token format and expiration (client-side only)
 * @param {string} token - JWT token to verify
 * @returns {Object} - Decoded payload if valid
 * @throws {Error} - If token is invalid or expired
 */
export function verifyJWT(token) {
  const parts = token.split('.');

  // 1. Check if token has exactly 3 parts
  if (parts.length !== 3) {
    throw new Error('Invalid JWT format');
  }

  const [encodedHeader, encodedPayload, signature] = parts;

  let payload;
  try {
    // 2. Decode payload
    payload = JSON.parse(atob(
      encodedPayload
        .replace(/-/g, '+')
        .replace(/_/g, '/')
        .padEnd(Math.ceil(encodedPayload.length / 4) * 4, '=')
    ));
  } catch (e) {
    throw new Error('Invalid payload format');
  }

  // 3. Check expiration date if present
  const currentTime = Math.floor(Date.now() / 1000); // Current time in seconds
  if (payload.e && currentTime > payload.e) {
    throw new Error('JWT expired');
  }

  // 4. Note: Actual signature verification happens on server
  
  // 5. Return payload if everything is correct
  return payload;
}

/**
 * Format a date object to a localized string
 * @param {Date|number} date - Date object or timestamp
 * @param {string} locale - Locale string (default: based on current language)
 * @returns {string} - Formatted date string
 */
export function formatDate(date, locale = getCurrentLanguage()) {
  // Convert to Date object if timestamp
  const dateObj = typeof date === 'number' ? new Date(date) : date;
  
  // Format based on locale
  return dateObj.toLocaleDateString(locale, {
    year: 'numeric',
    month: 'long',
    day: 'numeric'
  });
}

/**
 * Format a time object to a localized string
 * @param {Date|number} time - Date object or timestamp
 * @param {string} locale - Locale string (default: based on current language)
 * @returns {string} - Formatted time string
 */
export function formatTime(time, locale = getCurrentLanguage()) {
  // Convert to Date object if timestamp
  const dateObj = typeof time === 'number' ? new Date(time) : time;
  
  // Format based on locale
  return dateObj.toLocaleTimeString(locale, {
    hour: '2-digit',
    minute: '2-digit'
  });
}

/**
 * Get current language from various sources
 * @returns {string} - Language code (e.g., 'en')
 */
export function getCurrentLanguage() {
  // Check if the i18n system is available
  if (window.i18n && typeof window.i18n.getCurrentLanguage === 'function') {
    return window.i18n.getCurrentLanguage();
  }
  
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
 * Debounce function to limit how often a function runs
 * @param {Function} func - Function to debounce
 * @param {number} wait - Wait time in milliseconds
 * @returns {Function} - Debounced function
 */
export function debounce(func, wait = 300) {
  let timeout;
  
  return function executedFunction(...args) {
    const later = () => {
      clearTimeout(timeout);
      func(...args);
    };
    
    clearTimeout(timeout);
    timeout = setTimeout(later, wait);
  };
}

/**
 * Throttle function to limit function execution rate
 * @param {Function} func - Function to throttle
 * @param {number} limit - Throttle limit in milliseconds
 * @returns {Function} - Throttled function
 */
export function throttle(func, limit = 300) {
  let inThrottle;
  
  return function(...args) {
    if (!inThrottle) {
      func(...args);
      inThrottle = true;
      setTimeout(() => inThrottle = false, limit);
    }
  };
}

/**
 * Generate a random ID
 * @param {string} prefix - Optional prefix for the ID
 * @returns {string} - Random ID
 */
export function generateId(prefix = 'id') {
  return `${prefix}_${Math.random().toString(36).substring(2, 9)}`;
}

/**
 * Check if element is in viewport
 * @param {HTMLElement} el - Element to check
 * @param {number} offset - Offset from viewport edges (default: 0)
 * @returns {boolean} - True if element is in viewport
 */
export function isInViewport(el, offset = 0) {
  if (!el || !el.getBoundingClientRect) return false;
  
  const rect = el.getBoundingClientRect();
  
  return (
    rect.top >= 0 - offset &&
    rect.left >= 0 - offset &&
    rect.bottom <= (window.innerHeight || document.documentElement.clientHeight) + offset &&
    rect.right <= (window.innerWidth || document.documentElement.clientWidth) + offset
  );
}

/**
 * Add event listener with automatic removal when element is destroyed
 * @param {HTMLElement} element - Element to attach listener to
 * @param {string} eventType - Event type (e.g., 'click')
 * @param {Function} callback - Event handler
 * @param {boolean|Object} options - Event listener options
 */
export function addSafeEventListener(element, eventType, callback, options = false) {
  if (!element || !element.addEventListener) return;
  
  element.addEventListener(eventType, callback, options);
  
  // Use MutationObserver to detect when element is removed from DOM
  const observer = new MutationObserver((mutations) => {
    mutations.forEach((mutation) => {
      mutation.removedNodes.forEach((node) => {
        if (node === element || node.contains(element)) {
          element.removeEventListener(eventType, callback, options);
          observer.disconnect();
        }
      });
    });
  });
  
  // Start observing the document
  observer.observe(document.body, { childList: true, subtree: true });
}

// Add global utilities to window for use in inline handlers
window.verifyJWT = verifyJWT;
window.formatDate = formatDate;
window.formatTime = formatTime;
window.getCurrentLanguage = getCurrentLanguage;
