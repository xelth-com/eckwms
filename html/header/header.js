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

// Store the auto-show menu timeout ID so we can clear it
let autoShowMenuTimeout = null;

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
  applyButtonBackgrounds();
  initEventListeners();
  initMainMenuCards();


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
  if (langToggleBtn) {
    langToggleBtn.addEventListener('click', function (e) {
      e.preventDefault();
      e.stopPropagation();
      toggleLanguageGroup();
    });
    console.log('Added event listener to language toggle button from header.js');
  }

  // Add language selection events
  document.querySelectorAll('#langMenu [data-language]').forEach(button => {
    button.addEventListener('click', function () {
      const langCode = this.getAttribute('data-language');
      if (langCode && window.setLanguage) {
        resetMenuTimer();
        window.setLanguage(langCode);
      }
    });
  });
}

/**
 * Apply SVG backgrounds to buttons
 */
function applyButtonBackgrounds() {
  // Добавить проверку и ожидание 
  const waitForBackSvg = () => {
    return new Promise((resolve) => {
      const checkSvg = () => {
        if (window.backSvg2) {
          resolve(window.backSvg2);
        } else {
          setTimeout(checkSvg, 50); // Повторять проверку каждые 50мс
        }
      };
      checkSvg();
    });
  };

  waitForBackSvg().then(backSvg2 => {
    const backButtonImg = `url(data:image/svg+xml;charset=utf-8;base64,${btoa(backSvg2)})`;
    window.backButtonImg = backButtonImg;

    // Apply to all buttons
    Array.from(document.getElementsByClassName("button")).forEach(element => {
      element.style.backgroundImage = backButtonImg;
    });

    // Переинициализировать карточки меню после загрузки SVG
    initMainMenuCards();
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
 * Инициализация глобального списка языков
 * Вызывается один раз при загрузке модуля
 */
function initializeLanguagesList() {
  // Определяем глобальный список языков, если он еще не существует
  if (!window.allAvailableLanguages || window.allAvailableLanguages.length === 0) {

      window.allAvailableLanguages = [
        'en', 'de', 'tr', 'pl', 'fr', 'it', 'es', 'ru', 'ar', 'zh', 'ro', 'hr', 'bg', 'hi', 'ja', 'ko', 'cs',
        'nl', 'el', 'pt', 'he', 'hu', 'sv', 'da', 'fi', 'sk', 'lt', 'lv', 'et', 'sl', 'uk', 'sr', 'bs', 'no'
      ];
    

    console.log(`Инициализирован список из ${window.allAvailableLanguages.length} языков`);
  }

  return window.allAvailableLanguages;
}

/**
 * Получает текущий язык из различных источников
 * @returns {string} - Код языка (например, 'en')
 */
function getCurrentLanguage() {
  // Проверяем мета-тег (серверное значение)
  const metaTag = document.querySelector('meta[name="app-language"]');
  if (metaTag && metaTag.content) {
    return metaTag.content;
  }

  // Проверяем глобальную переменную
  if (typeof window.language !== 'undefined' && window.language) {
    return window.language;
  }

  // Если есть функция getCurrentLanguage в глобальном объекте i18n
  if (window.i18n && typeof window.i18n.getCurrentLanguage === 'function') {
    return window.i18n.getCurrentLanguage();
  }

  // Проверяем HTML атрибут lang
  const htmlLang = document.documentElement.lang;
  if (htmlLang) {
    return htmlLang;
  }

  // Проверяем i18next cookie
  const cookieMatch = document.cookie.match(/i18next=([^;]+)/);
  if (cookieMatch) {
    return cookieMatch[1];
  }

  // Проверяем localStorage
  try {
    const lsLang = localStorage.getItem('i18nextLng');
    if (lsLang) {
      return lsLang;
    }
  } catch (e) {
    // Игнорируем ошибки localStorage
  }

  // По умолчанию возвращаем английский
  return 'en';
}

/**
 * Инициализирует языковые кнопки, размещая текущий язык на позиции langMain1
 */
function initLanguageButtonsWithCurrentLanguage() {
  // Получаем текущий язык
  const currentLanguage = getCurrentLanguage();

  // Если язык не определен, выходим
  if (!currentLanguage) return;

  console.log(`Инициализация языковых кнопок с текущим языком: ${currentLanguage}`);

  // Убедимся, что список языков инициализирован
  const languages = window.allAvailableLanguages || initializeLanguagesList();

  // Находим индекс текущего языка в массиве
  const currentLangIndex = languages.indexOf(currentLanguage);
  if (currentLangIndex === -1) {
    console.warn(`Текущий язык ${currentLanguage} не найден в списке языков`);
    return;
  }

  // Определяем количество видимых кнопок
  let visibleCount = 0;
  for (let i = 17; i >= 1; i--) {
    const btn = document.getElementById(`langMain${i}`);
    if (btn) {
      const style = window.getComputedStyle(btn);
      if (style.display !== 'none') {
        visibleCount = i;
        break;
      }
    }
  }

  if (visibleCount === 0) {
    console.warn("Не удалось определить количество видимых кнопок");
    return;
  }

  // Создаем новый набор языков, начиная с текущего
  const newLanguages = [];
  for (let i = 0; i < visibleCount; i++) {
    const nextIndex = (currentLangIndex + i) % languages.length;
    const nextLang = languages[nextIndex];
    newLanguages.push(nextLang);
  }

  // Обновляем кнопки
  for (let i = 1; i <= visibleCount; i++) {
    const btn = document.getElementById(`langMain${i}`);
    if (btn) {
      const newLangIndex = i - 1;
      if (newLangIndex < newLanguages.length) {
        const newLang = newLanguages[newLangIndex];

        // Обновляем атрибут
        btn.setAttribute('data-language', newLang);

        // Обновляем SVG use
        const svgUse = btn.querySelector('svg use');
        if (svgUse) {
          svgUse.setAttribute('href', `#${newLang}`);
        }

        console.log(`Кнопка langMain${i} инициализирована с языком ${newLang}`);
      }
    }
  }

  console.log(`Инициализация языковых кнопок завершена, текущий язык ${currentLanguage} на позиции langMain1`);
}

/**
 * Reset menu timer when user interacts with language elements
 */
function resetMenuTimer() {
  // Set flag to indicate menu was interacted with
  window.menuUsed = true;
  
  // Clear existing timeout if it exists
  if (autoShowMenuTimeout) {
    clearTimeout(autoShowMenuTimeout);
    console.log("Menu auto-display timer reset due to user interaction");
  }
  
  // Set a new timeout (maintains the auto-display functionality but resets the timer)
  autoShowMenuTimeout = setTimeout(() => {
    // Reset the flag to allow auto-display again
    window.menuUsed = false;
    
    if (window.matchMedia("(min-width: 1001px)").matches) {
      const menuButtons = document.getElementById("mainMenuButtons");
      if (menuButtons && menuButtons.style.display === "none") {
        if (!window.menuUsed) {
          showMenu("mainMenu");
        }
      }
    }
  }, 30000); // Same 30-second delay as original
}

/**
 * Функция циклического переключения языковых кнопок
 * Поддерживает до 32 языковых кнопок
 */
function toggleLanguageGroup() {
  // Reset menu timer when toggling language group
  resetMenuTimer();
  
  console.log("Toggling language group");

  // Убедимся, что список языков инициализирован
  const languages = window.allAvailableLanguages || initializeLanguagesList();

  // Получаем все видимые кнопки языков
  // Определяем количество видимых кнопок и последний активный язык
  // Проверяем кнопки от langMain32 до langMain1 - поддерживает до 32 кнопок
  let visibleCount = 0;
  let lastActiveLanguage = null;

  for (let i = 17; i >= 1; i--) {
    const btn = document.getElementById(`langMain${i}`);
    if (btn) {
      const style = window.getComputedStyle(btn);
      if (style.display !== 'none') {
        // Найдена первая видимая кнопка с конца
        visibleCount = i;
        lastActiveLanguage = btn.getAttribute('data-language');
        console.log(`Первая видимая кнопка: langMain${i}, язык: ${lastActiveLanguage}`);
        break;
      }
    }
  }

  if (visibleCount === 0 || !lastActiveLanguage) {
    console.warn("Не удалось определить видимые кнопки или последний активный язык");
    return;
  }

  console.log(`Количество видимых кнопок: ${visibleCount}`);
  console.log(`Последний активный язык: ${lastActiveLanguage}`);

  // Находим индекс последнего активного языка в массиве языков
  const lastActiveIndex = languages.indexOf(lastActiveLanguage);
  if (lastActiveIndex === -1) {
    console.warn(`Язык ${lastActiveLanguage} не найден в списке языков`);
    return;
  }

  // Создаем новый набор языков, начиная со следующего после последнего активного
  const newLanguages = [];
  for (let i = 0; i < visibleCount; i++) {
    // Циклически берем следующий язык (+1 смещает нас к следующему)
    const nextIndex = (lastActiveIndex + i + 1) % languages.length;
    const nextLang = languages[nextIndex];
    newLanguages.push(nextLang);
  }

  console.log(`Новые языки (начиная с langMain1): ${newLanguages.join(', ')}`);

  // Обновляем языковые кнопки от langMain1 до langMain{visibleCount}
  for (let i = 1; i <= visibleCount; i++) {
    const btn = document.getElementById(`langMain${i}`);
    if (btn) {
      const newLangIndex = i - 1; // Индекс в массиве newLanguages
      if (newLangIndex < newLanguages.length) {
        const newLang = newLanguages[newLangIndex];

        // Обновляем атрибут
        btn.setAttribute('data-language', newLang);

        // Обновляем SVG use
        const svgUse = btn.querySelector('svg use');
        if (svgUse) {
          svgUse.setAttribute('href', `#${newLang}`);
        }

        console.log(`Кнопка langMain${i} теперь имеет язык ${newLang}`);
      }
    }
  }

  console.log("Ротация языков завершена");
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
      
      langMenu.style.setProperty('display', 'none', 'important');
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

  // Инициализируем кнопки языков с текущим языком на первой позиции
  initLanguageButtonsWithCurrentLanguage();

  // Initialize language selector
  if (typeof initLanguageSelector === 'function') {
    initLanguageSelector();
  } else if (window.syncLanguageMasks) {
    window.syncLanguageMasks();
  }

  // Add event listeners to language menu elements to reset timer
  const langMenu = document.getElementById('langMenu');
  if (langMenu) {
    langMenu.addEventListener('mouseover', resetMenuTimer);
  }
  
  // Add event listeners to language toggle button
  const langToggleBtn = document.getElementById('langToggleBtn');
  if (langToggleBtn) {
    langToggleBtn.addEventListener('click', resetMenuTimer);
  }
  
  // Add event listeners to all language buttons
  document.querySelectorAll('#langMenu [data-language]').forEach(button => {
    button.addEventListener('mouseover', resetMenuTimer);
    button.addEventListener('click', resetMenuTimer);
  });
  
  // Add event listeners to language groups
  const langGroup1 = document.getElementById('langGroup1');
  const langGroup2 = document.getElementById('langGroup2');
  if (langGroup1) langGroup1.addEventListener('mouseover', resetMenuTimer);
  if (langGroup2) langGroup2.addEventListener('mouseover', resetMenuTimer);

  // Auto-show main menu on desktop after delay if not used
  autoShowMenuTimeout = setTimeout(() => {
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

// Инициализируем список языков при загрузке модуля
initializeLanguagesList();

// Export functions for global access
window.showMenu = showMenu;
window.mainMenuCardOpen = mainMenuCardOpen;
window.mainMenuCardClose = mainMenuCardClose;
window.toggleLanguageGroup = toggleLanguageGroup;
window.initLanguageButtonsWithCurrentLanguage = initLanguageButtonsWithCurrentLanguage;
window.getCurrentLanguage = getCurrentLanguage;
window.resetMenuTimer = resetMenuTimer;