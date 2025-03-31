/**
 * Language Selector Module - Восстановлен из старой версии
 * Обрабатывает переключение языков и маски для языковых флагов
 */

/**
 * Переключает видимость всплывающего меню выбора языка
 * @param {string} popupType - Тип всплывающего меню ('eu' или 'un')
 */
export function toggleLanguagePopup(popupType) {
  const euPopup = document.getElementById('euPopup');
  const unPopup = document.getElementById('unPopup');
  
  // Закрыть все меню если они открыты
  if (popupType === 'eu') {
    if (euPopup.classList.contains('visible')) {
      euPopup.classList.remove('visible');
    } else {
      unPopup.classList.remove('visible'); // Закрыть другое меню
      euPopup.classList.add('visible');
      highlightCurrentLanguage(euPopup);
    }
  } else if (popupType === 'un') {
    if (unPopup.classList.contains('visible')) {
      unPopup.classList.remove('visible');
    } else {
      euPopup.classList.remove('visible'); // Закрыть другое меню
      unPopup.classList.add('visible');
      highlightCurrentLanguage(unPopup);
    }
  }
}

/**
 * Выделяет текущий выбранный язык во всплывающем меню
 * @param {HTMLElement} popup - Всплывающее меню
 */
function highlightCurrentLanguage(popup) {
  const currentLang = getCurrentLanguage();
  const buttons = popup.querySelectorAll('.langButton');
  
  buttons.forEach(button => {
    button.classList.remove('active');
    
    // Извлекаем код языка из обработчика клика
    const onclickAttr = button.getAttribute('onclick');
    if (onclickAttr) {
      const langMatch = onclickAttr.match(/selectLanguage\(['"]([a-z]{2})['"]\)/);
      if (langMatch && langMatch[1] === currentLang) {
        button.classList.add('active');
      }
    }
  });
}

/**
 * Получает текущий язык из разных источников
 * @returns {string} - Код языка (например 'en', 'de', 'fr')
 */
export function getCurrentLanguage() {
  // Проверяем глобальную переменную
  if (typeof window.language !== 'undefined' && window.language) {
    return window.language;
  }
  
  // Проверяем HTML lang атрибут
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
  
  // Возвращаем английский по умолчанию
  return 'en';
}

/**
 * Выбирает язык и обновляет маски языковых флагов
 * @param {string} langCode - Код языка (например 'en', 'de', 'fr')
 */
export function selectLanguage(langCode) {
  // Закрыть все всплывающие меню
  document.getElementById('euPopup').classList.remove('visible');
  document.getElementById('unPopup').classList.remove('visible');
  
  // Получаем текущий язык
  const previousLanguage = getCurrentLanguage();
  
  // Если выбран тот же язык, просто закрываем меню
  if (langCode === previousLanguage) {
    return;
  }
  
  // Обновляем маски SVG как в оригинальной функции
  try {
    document.getElementById(`${previousLanguage}Mask`).setAttribute("mask", "url(#maskClose)");
  } catch (e) { /* Игнорируем ошибки */ }
  
  try {
    document.getElementById(`${langCode}Mask`).setAttribute("mask", "url(#maskOpen)");
  } catch (e) { /* Игнорируем ошибки */ }
  
  // Используем функцию i18n, если она доступна
  if (window.i18n && window.i18n.changeLanguage) {
    window.i18n.changeLanguage(langCode);
  } else {
    // Резервный вариант: использовать оригинальную логику
    
    // Обновляем переменную языка
    window.language = langCode;
    window.menuUsed = true;
    
    // Сохраняем в cookie и localStorage
    document.cookie = `i18next=${langCode}; path=/; max-age=${60 * 60 * 24 * 365}`;
    try {
      localStorage.setItem('i18nextLng', langCode);
    } catch (e) { /* Игнорируем ошибки */ }
    
    // Перезагружаем страницу с новым языком
    const cacheBuster = Date.now();
    const url = new URL(window.location.href);
    url.searchParams.set('i18n_cb', cacheBuster);
    url.searchParams.set('lang', langCode);
    window.location.href = url.toString();
  }
}

/**
 * Оригинальная функция setLanguage из старой версии
 * @param {string} langPos - Идентификатор языка или элемента
 */
export function setLanguage(langPos) {
  // Проверяем специальные случаи для всплывающих меню
  if (langPos === 'lang2' || langPos === 'eu') {
    toggleLanguagePopup('eu');
    return;
  } else if (langPos === 'lang1' || langPos === 'un') {
    toggleLanguagePopup('un');
    return;
  }
  
  // Обрабатываем обычное переключение языка
  const previousLanguage = getCurrentLanguage();
  
  let newLanguage;
  if (langPos.slice(0, 4) === "lang") {
    window.menuUsed = true;
    newLanguage = document.getElementById(langPos).getAttribute("href").slice(1);
  } else {
    newLanguage = langPos;
  }
  
  if (newLanguage === previousLanguage) return;

  // Обновляем маски SVG
  try {
    document.getElementById(`${previousLanguage}Mask`).setAttribute("mask", "url(#maskClose)");
  } catch (e) { /* Игнорируем ошибки */ }
  
  try {
    document.getElementById(`${newLanguage}Mask`).setAttribute("mask", "url(#maskOpen)");
  } catch (e) { /* Игнорируем ошибки */ }

  // Обновляем переменную языка
  window.language = newLanguage;
  
  // Сохраняем в cookie и localStorage
  document.cookie = `i18next=${newLanguage}; path=/; max-age=${60 * 60 * 24 * 365}`;
  try {
    localStorage.setItem('i18nextLng', newLanguage);
  } catch (e) { /* Игнорируем ошибки */ }
  
  // Перезагружаем страницу с новым языком
  const cacheBuster = Date.now();
  const url = new URL(window.location.href);
  url.searchParams.set('i18n_cb', cacheBuster);
  url.searchParams.set('lang', newLanguage);
  window.location.href = url.toString();
}

/**
 * Инициализация обработчиков событий для кликов вне меню языков
 */
export function initLanguageSelector() {
  document.addEventListener('click', function(event) {
    const euPopup = document.getElementById('euPopup');
    const unPopup = document.getElementById('unPopup');
    
    // Проверка клика для EU меню
    const euButton = document.querySelector('[onclick="setLanguage(\'lang2\')"]');
    if (euPopup && euPopup.classList.contains('visible') && 
        !euPopup.contains(event.target) && 
        (!euButton || !euButton.contains(event.target))) {
      euPopup.classList.remove('visible');
    }
    
    // Проверка клика для UN меню
    const unButton = document.querySelector('[onclick="setLanguage(\'lang1\')"]');
    if (unPopup && unPopup.classList.contains('visible') && 
        !unPopup.contains(event.target) && 
        (!unButton || !unButton.contains(event.target))) {
      unPopup.classList.remove('visible');
    }
  });
}

// Инициализация при загрузке модуля
document.addEventListener('DOMContentLoaded', initLanguageSelector);

// Экспортируем функции в глобальное пространство для инлайн-обработчиков
window.toggleLanguagePopup = toggleLanguagePopup;
window.selectLanguage = selectLanguage;
window.setLanguage = setLanguage;