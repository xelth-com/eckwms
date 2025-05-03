/**
 * Header Module
 * Contains site logo, language selector, and main navigation menu
 */

import { loadCSS, loadTemplate } from '/core/module-loader.js';
// Зависимости от language-selector.js, если он нужен для UI кнопок
import { syncLanguageMasks, initLanguageSelector } from '/i18n/language-selector.js';
import './menu-permissions.js';

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
      toggleLanguageGroup(); // Эта функция осталась, но getCurrentLanguage внутри неё будет использовать window.i18n
    });
    console.log('Added event listener to language toggle button from header.js');
  }

  // Add language selection events
  document.querySelectorAll('#langMenu [data-language]').forEach(button => {
    button.addEventListener('click', function () {
      const langCode = this.getAttribute('data-language');
      // Используем глобальную функцию смены языка из i18n.js
      if (langCode && window.i18n && typeof window.i18n.changeLanguage === 'function') {
        resetMenuTimer();
        window.i18n.changeLanguage(langCode);
      } else if (langCode && window.setLanguage) { // Поддержка старого window.setLanguage на всякий случай
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

// --- ЛОКАЛЬНАЯ ФУНКЦИЯ getCurrentLanguage УДАЛЕНА ---

/**
 * Инициализирует языковые кнопки, размещая текущий язык на позиции langMain1
 */
function initLanguageButtonsWithCurrentLanguage() {
  // --- ИЗМЕНЕНИЕ ЗДЕСЬ ---
  // Пытаемся получить язык из ГЛОБАЛЬНОГО i18n объекта
  let currentLanguage = 'en'; // Язык по умолчанию
  if (window.i18n && typeof window.i18n.getCurrentLanguage === 'function') {
      try {
          currentLanguage = window.i18n.getCurrentLanguage();
          // Обработка случая, если функция вернула null/undefined
          if (!currentLanguage) {
              console.warn('window.i18n.getCurrentLanguage() вернула null/undefined, используется "en".');
              currentLanguage = 'en';
          }
      } catch (error) {
          console.error('Ошибка при вызове window.i18n.getCurrentLanguage():', error, 'Используется "en".');
          currentLanguage = 'en'; // Убедимся, что язык по умолчанию установлен при ошибке
      }
  } else {
      // Сообщение об ошибке, если i18n или функция недоступны
      console.error('window.i18n или window.i18n.getCurrentLanguage недоступны! Используется язык по умолчанию "en".');
      // currentLanguage уже 'en'
  }
  // --- КОНЕЦ ИЗМЕНЕНИЯ ---

  console.log(`Инициализация языковых кнопок с текущим языком (из i18n или fallback): ${currentLanguage}`);

  // Убедимся, что список языков инициализирован
  const languages = window.allAvailableLanguages || initializeLanguagesList();

  // Находим индекс текущего языка в массиве
  // Теперь currentLanguage будет нормализован (например, "de"), так что поиск должен сработать
  const currentLangIndex = languages.indexOf(currentLanguage);
  if (currentLangIndex === -1) {
    // Эта ошибка теперь менее вероятна, но оставим на всякий случай
    console.warn(`Текущий язык ${currentLanguage} (из i18n) не найден в списке window.allAvailableLanguages`);
    return; // Выходим, если язык не найден в списке
  }

  // Определяем количество видимых кнопок
  let visibleCount = 0;
  for (let i = 17; i >= 1; i--) { // Проверяем кнопки с langMain17 до langMain1
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
    console.warn("Не удалось определить количество видимых кнопок для инициализации");
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

        //console.log(`Кнопка langMain${i} инициализирована с языком ${newLang}`); // Можно раскомментировать для отладки
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
    //console.log("Menu auto-display timer reset due to user interaction"); // Можно раскомментировать для отладки
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
  // Проверяем кнопки от langMain17 до langMain1 - адаптировано под HTML
  let visibleCount = 0;
  let lastActiveLanguage = null;

  // Ищем последнюю видимую кнопку, чтобы определить конец текущего набора
  for (let i = 17; i >= 1; i--) {
    const btn = document.getElementById(`langMain${i}`);
    if (btn) {
      const style = window.getComputedStyle(btn);
      if (style.display !== 'none') {
        // Найдена последняя видимая кнопка
        visibleCount = i; // Количество видимых кнопок = номер последней видимой
        lastActiveLanguage = btn.getAttribute('data-language');
        console.log(`Найдена последняя видимая кнопка: langMain${i}, язык: ${lastActiveLanguage}`);
        break; // Прерываем цикл, так как нашли последнюю видимую
      }
    }
  }

  if (visibleCount === 0 || !lastActiveLanguage) {
    console.warn("Не удалось определить видимые кнопки или последний активный язык для переключения");
    return;
  }

  console.log(`Количество видимых кнопок для переключения: ${visibleCount}`);
  console.log(`Последний активный язык в текущем наборе: ${lastActiveLanguage}`);

  // Находим индекс последнего активного языка в общем массиве языков
  const lastActiveIndex = languages.indexOf(lastActiveLanguage);
  if (lastActiveIndex === -1) {
    console.warn(`Язык ${lastActiveLanguage} не найден в списке window.allAvailableLanguages`);
    return;
  }

  // Создаем новый набор языков, начиная со следующего после последнего активного
  const newLanguages = [];
  for (let i = 0; i < visibleCount; i++) {
    // Циклически берем следующий язык (+1 смещает нас к следующему за последним видимым)
    const nextIndex = (lastActiveIndex + 1 + i) % languages.length;
    const nextLang = languages[nextIndex];
    newLanguages.push(nextLang);
  }

  console.log(`Новые языки для отображения (начиная с langMain1): ${newLanguages.join(', ')}`);

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

        // console.log(`Кнопка langMain${i} теперь имеет язык ${newLang}`); // Можно раскомментировать для отладки
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

  // Find language menu (specific to mainMenu)
  const langMenu = (menuType === "mainMenu") ? document.getElementById("langMenu") : null;

  // Toggle menu visibility
  if (buttonsElement.style.display !== "none") {

    // Closing menu
    window.waitForTransition = true;
    setTimeout(() => {
      buttonsElement.style.display = "none";

      // Special handling for main menu to show language menu
      if (menuType === "mainMenu" && langMenu) {
         // Убираем display: none !important, если он был установлен при открытии
         langMenu.style.removeProperty('display');
         // Показываем langMenu как inline-block (или flex, если нужно)
         langMenu.style.display = "inline-block"; // Или 'flex' в зависимости от CSS
      }

      window.waitForTransition = false;
    }, 3000); // Delay before hiding

  } else {
    // Opening menu
    window.waitForTransition = true; // Set transition lock immediately

    // Hide language menu *before* showing main menu buttons
    if (menuType === "mainMenu" && langMenu) {
       // Устанавливаем display: none !important, чтобы переопределить стили
       langMenu.style.setProperty('display', 'none', 'important');
    }
    buttonsElement.style.display = "inline-block";

    // Reset transition lock after a short delay to allow animations to start
    setTimeout(() => {
        window.waitForTransition = false;
    }, 50); // Small delay
  }

  // Animate menu lines
  if (elements.length > 1 && elements[1].getAttribute("x") === "10") {
    // Open menu animation
    elements[1].setAttribute("x", "65"); // Adjust based on your SVG structure
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
    elements[1].setAttribute("x", "10"); // Adjust based on your SVG structure
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
  let i = 0; // Index of the card to potentially reuse

  // Find an unused card or the card already showing this menu
  let foundExisting = false;
  cards.forEach((element, index) => {
    if (element.mmn === mainMenuNumber) { // Card already shows this menu
      equal = true;
      clearTimeout(element.timeoutId);
      clearTimeout(element.timeoutId1);
      i = index; // Mark this card index
      foundExisting = true;
    }
    const z = parseInt(element.el.style.zIndex) || 0;
    if (zmax < z) {
      zmax = z;
    }
    // Keep track of the lowest z-index card *that is not currently assigned* (mmn === 'empty' or '')
    if (!foundExisting && (!element.mmn || element.mmn === 'empty') && (zmin >= z)) {
       zmin = z;
       i = index;
    }
  });

  // If already showing this menu, just ensure its z-index is highest and return
  if (equal) {
     cards[i].el.style.zIndex = `${zmax + 1}`;
     return;
  }

  // --- Card Reuse/Assignment Logic ---
  // We use card 'i', which is either the lowest z-index available card or the card found for reuse
  const cardToUse = cards[i];

  // Hide other *active* cards (except the one we are reusing/opening)
  cards.forEach((element, index) => {
    if (index !== i && element.mmn && element.mmn !== 'empty') { // Only hide cards that have content
      element.el.style.opacity = "0";
      element.el.style.filter = "blur(10px)";
      element.el.style.display = "none"; // Hide completely after transition
      element.mmn = "empty"; // Mark as empty
      element.el.onmouseenter = null;
      element.el.onmouseleave = null;
    }
  });


  // Clear timeouts and configure the chosen card
  clearTimeout(cardToUse.timeoutId);
  clearTimeout(cardToUse.timeoutId1);

  cardToUse.el.style.zIndex = `${zmax + 1}`;
  cardToUse.el.style.display = "block"; // Make sure it's visible
  cardToUse.el.onmouseenter = () => mainMenuCardOpen(mainMenuNumber);
  cardToUse.el.onmouseleave = () => mainMenuCardClose(mainMenuNumber);

  // Get content from hidden div
  const hiddenDiv = menu.querySelector('div[hidden]');
  if (hiddenDiv) {
    cardToUse.el.innerHTML = hiddenDiv.innerHTML;
    // Re-attach event listeners to dynamically added content inside the card if needed
    // Example: cardToUse.el.querySelectorAll('[onclick]').forEach(el => /* attach listener */);
  } else {
    cardToUse.el.innerHTML = ''; // Clear content if source div is missing
  }

  cardToUse.mmn = mainMenuNumber; // Assign the menu number to the card

  // Position card (improved)
  const event = window.event; // Note: window.event is deprecated, consider passing event explicitly
  const cardRect = cardToUse.el.getBoundingClientRect();
  const menuRect = menu.getBoundingClientRect();

  let left = menuRect.left; // Default to align with menu item
  let top = menuRect.bottom + 5; // Default below menu item

  // Adjust if card goes off-screen
  if (left + cardRect.width > window.innerWidth) {
      left = window.innerWidth - cardRect.width - 10; // Adjust with padding
  }
  if (left < 0) {
      left = 10;
  }
  if (top + cardRect.height > window.innerHeight) {
      top = window.innerHeight - cardRect.height - 10;
  }
   if (top < 0) {
      top = 10;
   }

  cardToUse.el.style.left = `${left}px`;
  cardToUse.el.style.top = `${top}px`;


  // Use requestAnimationFrame for smoother transition start
  requestAnimationFrame(() => {
      cardToUse.el.style.opacity = "1";
      cardToUse.el.style.filter = "blur(0px)";
  });
}


/**
 * Close main menu card
 * @param {string} mainMenuNumber - Main menu ID
 */
export function mainMenuCardClose(mainMenuNumber) {
  const menu = document.getElementById(mainMenuNumber);
  if (!menu) return;

  menu.style.backgroundColor = "#ba80"; // Reset background of the trigger element

  cards.forEach((element) => {
    if (element.mmn === mainMenuNumber) {
       // Clear any pending close timeouts for this card
       clearTimeout(element.timeoutId);
       clearTimeout(element.timeoutId1);

       // Start fade out timeout
       element.timeoutId = setTimeout(() => {
         element.el.style.opacity = "0";
         element.el.style.filter = "blur(10px)";

         // Set timeout to hide the element after fade out transition
         element.timeoutId1 = setTimeout(() => {
           element.el.style.display = "none";
           element.mmn = "empty"; // Mark as empty *after* hiding
           element.el.onmouseenter = null; // Clean up listeners
           element.el.onmouseleave = null;
         }, 500); // Should match transition duration in CSS
       }, 1000); // Delay before starting fade out
    }
  });
}


/**
 * Fix language toggle button CSS (Injected CSS - consider moving to header.css)
 */
function fixLanguageToggleCSS() {
  // Create style element for fixes
  const styleElement = document.createElement('style');
  styleElement.textContent = `
    /* Fix language menu layout */
    #langMenu {
      display: flex !important; /* Use flex for better alignment */
      flex-wrap: nowrap !important;
      align-items: center !important;
      margin-left: auto; /* Pushes it to the right in flex container */
      padding: 3px;
      flex-direction: row-reverse !important; /* <<< ВОЗВРАЩАЕМ RTL ДЛЯ ВСЕГО МЕНЮ */
    }

    .langButtonGroup {
      display: flex !important;
      flex-wrap: nowrap !important;
      flex-direction: row-reverse !important; /* <<< ВОЗВРАЩАЕМ RTL ДЛЯ ГРУПП КНОПОК */
    }

    #langToggleBtn {
      display: inline-block !important;
      margin: 0 3px;
      vertical-align: middle;
      order: 1 !important; /* <<< ВОЗВРАЩАЕМ order: 1 (или подбери нужное значение для RTL) */ /* В RTL flex-контейнере order: 1 может ставить элемент в конец (визуально слева) */
    }

    #langMenu .button {
      margin: 0 2px; /* Spacing between buttons */
      vertical-align: middle;
      flex-shrink: 0; /* Prevent buttons from shrinking */
    }

    /* Ensure only one group is visible initially (JS handles this, but keep for safety) */
    #langGroup1 {
       display: flex !important; /* Изначально показываем первую группу */
    }

    #langGroup2 {
       display: none !important; /* Вторую скрываем */
    }
  `;
  // Check if style already exists to prevent duplicates
  if (!document.getElementById('header-lang-fixes')) {
      styleElement.id = 'header-lang-fixes';
      document.head.appendChild(styleElement);
  }
}

/**
 * Post-initialization tasks
 */
export function postInit() {
  // Fix language toggle button CSS
  fixLanguageToggleCSS(); // Consider moving styles to CSS file

  // Инициализация разрешений меню
  if (window.renderPermittedMenuItems) {
    window.renderPermittedMenuItems();
  }

  // ИНИЦИАЛИЗИРУЕМ КНОПКИ ЯЗЫКОВ - ТЕПЕРЬ ИСПОЛЬЗУЕТ window.i18n
  initLanguageButtonsWithCurrentLanguage();

  // Initialize language selector UI component if it exists
  if (typeof initLanguageSelector === 'function') {
    initLanguageSelector();
  } else if (window.syncLanguageMasks) {
    // Fallback or alternative sync mechanism? Clarify which syncLanguageMasks to use.
    // Assuming the one from language-selector.js is preferred if initLanguageSelector exists
    // Otherwise, use the global one if available
    if (typeof syncLanguageMasks !== 'function') { // check if imported one exists
         window.syncLanguageMasks(); // use global one from i18n.js
    } else {
         syncLanguageMasks(); // use imported one
    }
  }

  // Add event listeners to language menu elements to reset timer
  const langMenu = document.getElementById('langMenu');
  if (langMenu) {
    // Use mouseenter instead of mouseover to avoid excessive triggers
    langMenu.addEventListener('mouseenter', resetMenuTimer);
  }

  // Add event listeners to language toggle button
  const langToggleBtn = document.getElementById('langToggleBtn');
  if (langToggleBtn) {
    langToggleBtn.addEventListener('click', resetMenuTimer);
    langToggleBtn.addEventListener('mouseenter', resetMenuTimer); // Also reset on hover
  }

  // Add event listeners to all language buttons
  document.querySelectorAll('#langMenu [data-language]').forEach(button => {
    button.addEventListener('mouseenter', resetMenuTimer);
    button.addEventListener('click', resetMenuTimer);
  });

  // Add event listeners to language groups
  const langGroup1 = document.getElementById('langGroup1');
  const langGroup2 = document.getElementById('langGroup2');
  if (langGroup1) langGroup1.addEventListener('mouseenter', resetMenuTimer);
  if (langGroup2) langGroup2.addEventListener('mouseenter', resetMenuTimer);

  // Auto-show main menu on desktop after delay if not used
  if (autoShowMenuTimeout) clearTimeout(autoShowMenuTimeout); // Clear any previous timeouts
  autoShowMenuTimeout = setTimeout(() => {
    // Check media query inside timeout, in case window resized
    if (window.matchMedia("(min-width: 1001px)").matches) {
      const menuButtons = document.getElementById("mainMenuButtons");
      // Check menuUsed flag inside timeout as well
      if (menuButtons && menuButtons.style.display === "none" && !window.menuUsed) {
           showMenu("mainMenu");
      }
    }
  }, 30000);
}

// Инициализируем список языков при загрузке модуля
initializeLanguagesList();

// Export functions for potential external use (or legacy compatibility)
// Consider removing these if not strictly needed and relying on module imports
window.showMenu = showMenu;
window.mainMenuCardOpen = mainMenuCardOpen;
window.mainMenuCardClose = mainMenuCardClose;
window.toggleLanguageGroup = toggleLanguageGroup;
// window.initLanguageButtonsWithCurrentLanguage = initLanguageButtonsWithCurrentLanguage; // Might not need global exposure
// window.getCurrentLanguage = getCurrentLanguage; // DEFINITELY REMOVE THIS
window.resetMenuTimer = resetMenuTimer;

console.log("Header module loaded and initialized."); // Add log to confirm load