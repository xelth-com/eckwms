/**
 * Head Module
 * Sets up the document <head> with meta tags, CSS, and essential scripts
 */

import { loadCSS } from '/core/module-loader.js';



/**
 * Загружает необходимые пространства имен для текущей страницы
 * @param {string} pagePath - Путь текущей страницы
 */
function loadRequiredNamespaces(pagePath) {
  if (!window.i18n) return;
  
  // Базовые пространства имен, которые нужны на всех страницах
  const baseNamespaces = ['common'];
  
  // Специфичные для страниц пространства имен
  const pageSpecificNamespaces = {
    '/': ['common'],                  // Главная страница
    '/rma': ['rma', 'common'],        // Страница RMA
    '/status': ['common', 'status'],  // Страница статуса
    '/admin': ['common', 'admin'],    // Админ-панель
    '/auth': ['common', 'auth']       // Страница аутентификации
  };
  
  // Получаем список необходимых пространств имен для текущей страницы
  const pathNamespaces = pageSpecificNamespaces[pagePath] || [];
  
  // Объединяем базовые и специфичные пространства имен, убираем дубликаты
  const namespacesToLoad = [...new Set([...baseNamespaces, ...pathNamespaces])];
  
  console.log(`[i18n] Preloading namespaces for page ${pagePath}: ${namespacesToLoad.join(', ')}`);
  
  // Добавляем мета-тег с информацией о требуемых пространствах имен
  let nsMetaTag = document.querySelector('meta[name="i18n-namespaces"]');
  if (!nsMetaTag) {
    nsMetaTag = document.createElement('meta');
    nsMetaTag.name = 'i18n-namespaces';
    document.head.appendChild(nsMetaTag);
  }
  nsMetaTag.content = namespacesToLoad.join(',');
  
  // Загружаем пространства имен через i18n API
  if (typeof window.i18n.loadNamespaces === 'function') {
    window.i18n.loadNamespaces(namespacesToLoad)
      .then(() => {
        console.log(`[i18n] Successfully loaded namespaces: ${namespacesToLoad.join(', ')}`);
      })
      .catch(err => {
        console.error(`[i18n] Error loading namespaces: ${err.message}`);
      });
  }
}




/**
 * Initialize the document head
 */
export function init() {
  // Set page title
  document.title = 'Eck - M3 Mobile GmbH';
  
  // Set meta tags
  setMetaTags();
  
  // Load favicon
  setFavicons();
  
  // Load essential stylesheets
  loadEssentialStyles();
  
  // Set root CSS variables
  setRootVariables();
  
  // Load essential scripts
  loadEssentialScripts();
  
  // Preload required namespaces for this page
  if (window.i18n) {
    loadRequiredNamespaces(window.location.pathname);
  } else {
    // Если i18n еще не загружен, ждем события инициализации
    document.addEventListener('i18n:initialized', function() {
      loadRequiredNamespaces(window.location.pathname);
    });
  }
}

/**
 * Set meta tags for SEO and responsiveness
 */
function setMetaTags() {
  // Set viewport meta
  let metaViewport = document.querySelector('meta[name="viewport"]');
  if (!metaViewport) {
    metaViewport = document.createElement('meta');
    metaViewport.name = 'viewport';
    document.head.appendChild(metaViewport);
  }
  metaViewport.content = 'width=device-width, initial-scale=1.0, user-scalable=yes';
  
  // Set content type
  let metaContentType = document.querySelector('meta[http-equiv="Content-Type"]');
  if (!metaContentType) {
    metaContentType = document.createElement('meta');
    metaContentType.httpEquiv = 'Content-Type';
    document.head.appendChild(metaContentType);
  }
  metaContentType.content = 'text/html; charset=utf-8';
  
  // Set theme color
  let metaThemeColor = document.querySelector('meta[name="theme-color"]');
  if (!metaThemeColor) {
    metaThemeColor = document.createElement('meta');
    metaThemeColor.name = 'theme-color';
    document.head.appendChild(metaThemeColor);
  }
  metaThemeColor.content = '#ffffff';
  
  // Set MS application tile color
  let metaMsTileColor = document.querySelector('meta[name="msapplication-TileColor"]');
  if (!metaMsTileColor) {
    metaMsTileColor = document.createElement('meta');
    metaMsTileColor.name = 'msapplication-TileColor';
    document.head.appendChild(metaMsTileColor);
  }
  metaMsTileColor.content = '#da532c';
  
  // Set language meta tag (for i18n system)
  const currentLang = getCurrentLanguage();
  let metaLang = document.querySelector('meta[name="app-language"]');
  if (!metaLang) {
    metaLang = document.createElement('meta');
    metaLang.name = 'app-language';
    document.head.appendChild(metaLang);
  }
  metaLang.content = currentLang;
  document.documentElement.lang = currentLang;
}

/**
 * Set favicons
 */
function setFavicons() {
  // Apple touch icon
  let appleTouchIcon = document.querySelector('link[rel="apple-touch-icon"]');
  if (!appleTouchIcon) {
    appleTouchIcon = document.createElement('link');
    appleTouchIcon.rel = 'apple-touch-icon';
    document.head.appendChild(appleTouchIcon);
  }
  appleTouchIcon.sizes = '180x180';
  appleTouchIcon.href = '/apple-touch-icon.png';
  
  // Favicon 32x32
  let favicon32 = document.querySelector('link[rel="icon"][sizes="32x32"]');
  if (!favicon32) {
    favicon32 = document.createElement('link');
    favicon32.rel = 'icon';
    favicon32.type = 'image/png';
    document.head.appendChild(favicon32);
  }
  favicon32.sizes = '32x32';
  favicon32.href = '/favicon-32x32.png';
  
  // Favicon 16x16
  let favicon16 = document.querySelector('link[rel="icon"][sizes="16x16"]');
  if (!favicon16) {
    favicon16 = document.createElement('link');
    favicon16.rel = 'icon';
    favicon16.type = 'image/png';
    document.head.appendChild(favicon16);
  }
  favicon16.sizes = '16x16';
  favicon16.href = '/favicon-16x16.png';
  
  // Web manifest
  let webManifest = document.querySelector('link[rel="manifest"]');
  if (!webManifest) {
    webManifest = document.createElement('link');
    webManifest.rel = 'manifest';
    document.head.appendChild(webManifest);
  }
  webManifest.href = '/site.webmanifest';
  
  // Safari pinned tab
  let safariPinnedTab = document.querySelector('link[rel="mask-icon"]');
  if (!safariPinnedTab) {
    safariPinnedTab = document.createElement('link');
    safariPinnedTab.rel = 'mask-icon';
    document.head.appendChild(safariPinnedTab);
  }
  safariPinnedTab.href = '/safari-pinned-tab.svg';
  safariPinnedTab.color = '#5bbad5';
}

/**
 * Load essential stylesheets
 */
function loadEssentialStyles() {
  loadCSS('/common/global.css');
  loadCSS('/i18n/i18n.css');
}

/**
 * Set CSS root variables
 */
function setRootVariables() {
  document.documentElement.style.setProperty('--fontSize', '20px');
  document.documentElement.style.setProperty('--lineHeight', '30px');
  document.documentElement.style.setProperty('--textShadow1', 
    '-0.4px -0.6px 0.3px rgb(160, 160, 160, 0.6), 0.4px 0.6px 0.3px rgb(70, 70, 70, 0.6), ' +
    '-0.8px -1.2px 3px rgb(215, 215, 215, 0.8), 1.6px 2.2px 4px rgb(0, 0, 0, 0.8), ' +
    '-2.4px -3.3px 8px rgb(255, 255, 255), 3.2px 4.4px 5px rgb(255, 255, 255)');
}

/**
 * Load essential scripts
 */
function loadEssentialScripts() {
  // Load translation utilities script
  loadScript('/js/translationUtils.js');
  
  // Load i18n script
  loadScript('/js/i18n.js');
}

/**
 * Utility to load a script
 * @param {string} src - Script source URL
 * @returns {Promise} - Promise that resolves when script is loaded
 */
function loadScript(src) {
  return new Promise((resolve, reject) => {
    // Check if already loaded
    if (document.querySelector(`script[src="${src}"]`)) {
      resolve();
      return;
    }
    
    // Create and add script element
    const script = document.createElement('script');
    script.src = src;
    script.defer = true;
    
    script.onload = () => resolve();
    script.onerror = (error) => {
      console.error(`Failed to load script: ${src}`, error);
      reject(error);
    };
    
    document.head.appendChild(script);
  });
}

/**
 * Get the current language from various sources
 * @returns {string} - Language code (e.g., 'en')
 */
function getCurrentLanguage() {
  // Check URL parameter
  const urlParams = new URLSearchParams(window.location.search);
  const langParam = urlParams.get('lang');
  if (langParam) {
    return langParam;
  }
  
  // Check cookie
  const langCookie = document.cookie.match(/i18next=([^;]+)/);
  if (langCookie) {
    return langCookie[1];
  }
  
  // Check localStorage
  try {
    const storedLang = localStorage.getItem('i18nextLng');
    if (storedLang) {
      return storedLang;
    }
  } catch (e) {
    // Silent fallback
  }
  
  // Default language
  return 'en';
}

// Run the initialization when the module is loaded
init();
