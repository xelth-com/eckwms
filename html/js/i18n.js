// html/js/i18n.js

/**
 * Модуль для управления мультиязычностью на клиентской стороне
 */
(function() {
  // Текущий язык (берем из cookie или localStorage, иначе язык браузера или 'de' по умолчанию)
  let currentLanguage = 
    getCookie('i18next') || 
    localStorage.getItem('i18nextLng') || 
    navigator.language.split('-')[0] || 
    'de';
  
  // Проверка поддерживаемых языков
  const supportedLanguages = [
    // Официальные языки ЕС
    'de', 'en', 'fr', 'it', 'es', 'pt', 'nl', 'da', 'sv', 'fi', 
    'el', 'cs', 'pl', 'hu', 'sk', 'sl', 'et', 'lv', 'lt', 'ro', 
    'bg', 'hr', 'ga', 'mt',
    // Дополнительные языки
    'ru', 'tr', 'ar', 'zh', 'uk', 'sr', 'he', 'ko', 'ja'
  ];
  
  // Если текущий язык не поддерживается, используем 'de' по умолчанию
  if (!supportedLanguages.includes(currentLanguage)) {
    currentLanguage = 'de';
  }
  
  // Кэш для переводов
  const translationCache = {};
  
  /**
   * Инициализация модуля
   */
  function init() {
    // Устанавливаем атрибут lang для HTML
    document.documentElement.lang = currentLanguage;
    
    // Если это один из языков RTL, добавляем соответствующий атрибут
    if (['ar', 'he'].includes(currentLanguage)) {
      document.documentElement.dir = 'rtl';
    } else {
      document.documentElement.dir = 'ltr';
    }
    
    // Сохраняем язык в cookie и localStorage
    setCookie('i18next', currentLanguage, 365); // на 365 дней
    localStorage.setItem('i18nextLng', currentLanguage);
    
    // Инициализируем переводы на странице
    updatePageTranslations();
    
    // Обработчик события для динамического содержимого
    document.addEventListener('DOMContentLoaded', function() {
      setupLanguageSwitcher();
    });
  }
  
  /**
   * Настройка переключателя языков
   */
  function setupLanguageSwitcher() {
    // Ищем селектор языка
    const languageSelector = document.querySelector('.language-selector');
    if (!languageSelector) return;
    
    // Очищаем содержимое
    languageSelector.innerHTML = '';
    
    // Флаги и названия для основных языков
    const languages = [
      { code: 'de', name: 'Deutsch', flag: '🇩🇪' },
      { code: 'en', name: 'English', flag: '🇬🇧' },
      { code: 'fr', name: 'Français', flag: '🇫🇷' },
      { code: 'it', name: 'Italiano', flag: '🇮🇹' },
      { code: 'es', name: 'Español', flag: '🇪🇸' },
      { code: 'ru', name: 'Русский', flag: '🇷🇺' },
      { code: 'zh', name: '中文', flag: '🇨🇳' },
      { code: 'ar', name: 'العربية', flag: '🇸🇦' },
      { code: 'ja', name: '日本語', flag: '🇯🇵' }
      // Можно добавить больше языков при необходимости
    ];
    
    // Создаем опции селектора
    languages.forEach(lang => {
      const option = document.createElement('div');
      option.className = 'language-option';
      option.dataset.lang = lang.code;
      option.innerHTML = `${lang.flag} ${lang.name}`;
      
      if (lang.code === currentLanguage) {
        option.classList.add('active');
      }
      
      option.addEventListener('click', function() {
        changeLanguage(lang.code);
      });
      
      languageSelector.appendChild(option);
    });
    
    // Кнопка "Ещё языки"
    const moreBtn = document.createElement('div');
    moreBtn.className = 'language-more-btn';
    moreBtn.textContent = '...';
    moreBtn.addEventListener('click', function() {
      showAllLanguages(languageSelector);
    });
    
    languageSelector.appendChild(moreBtn);
  }
  
  /**
   * Показать модальное окно со всеми доступными языками
   */
  function showAllLanguages(container) {
    // Полный список языков
    const allLanguages = [
      // ЕС
      { code: 'de', name: 'Deutsch', flag: '🇩🇪' },
      { code: 'en', name: 'English', flag: '🇬🇧' },
      { code: 'fr', name: 'Français', flag: '🇫🇷' },
      { code: 'it', name: 'Italiano', flag: '🇮🇹' },
      { code: 'es', name: 'Español', flag: '🇪🇸' },
      { code: 'pt', name: 'Português', flag: '🇵🇹' },
      { code: 'nl', name: 'Nederlands', flag: '🇳🇱' },
      { code: 'da', name: 'Dansk', flag: '🇩🇰' },
      { code: 'sv', name: 'Svenska', flag: '🇸🇪' },
      { code: 'fi', name: 'Suomi', flag: '🇫🇮' },
      { code: 'el', name: 'Ελληνικά', flag: '🇬🇷' },
      { code: 'cs', name: 'Čeština', flag: '🇨🇿' },
      { code: 'pl', name: 'Polski', flag: '🇵🇱' },
      { code: 'hu', name: 'Magyar', flag: '🇭🇺' },
      { code: 'sk', name: 'Slovenčina', flag: '🇸🇰' },
      { code: 'sl', name: 'Slovenščina', flag: '🇸🇮' },
      { code: 'et', name: 'Eesti', flag: '🇪🇪' },
      { code: 'lv', name: 'Latviešu', flag: '🇱🇻' },
      { code: 'lt', name: 'Lietuvių', flag: '🇱🇹' },
      { code: 'ro', name: 'Română', flag: '🇷🇴' },
      { code: 'bg', name: 'Български', flag: '🇧🇬' },
      { code: 'hr', name: 'Hrvatski', flag: '🇭🇷' },
      { code: 'ga', name: 'Gaeilge', flag: '🇮🇪' },
      { code: 'mt', name: 'Malti', flag: '🇲🇹' },
      // Дополнительные языки
      { code: 'ru', name: 'Русский', flag: '🇷🇺' },
      { code: 'tr', name: 'Türkçe', flag: '🇹🇷' },
      { code: 'ar', name: 'العربية', flag: '🇸🇦' },
      { code: 'zh', name: '中文', flag: '🇨🇳' },
      { code: 'uk', name: 'Українська', flag: '🇺🇦' },
      { code: 'sr', name: 'Српски', flag: '🇷🇸' },
      { code: 'he', name: 'עברית', flag: '🇮🇱' },
      { code: 'ko', name: '한국어', flag: '🇰🇷' },
      { code: 'ja', name: '日本語', flag: '🇯🇵' }
    ];
    
    // Создаем модальное окно
    const modal = document.createElement('div');
    modal.className = 'language-modal';
    modal.innerHTML = `
      <div class="language-modal-content">
        <div class="language-modal-header">
          <h3>Sprache auswählen / Select Language</h3>
          <button class="language-modal-close">&times;</button>
        </div>
        <div class="language-modal-body">
          <div class="language-grid"></div>
        </div>
      </div>
    `;
    
    // Добавляем стили
    const style = document.createElement('style');
    style.textContent = `
      .language-modal {
        position: fixed;
        top: 0;
        left: 0;
        width: 100%;
        height: 100%;
        background-color: rgba(0, 0, 0, 0.5);
        display: flex;
        justify-content: center;
        align-items: center;
        z-index: 1000;
      }
      .language-modal-content {
        background-color: white;
        padding: 20px;
        border-radius: 8px;
        max-width: 80%;
        max-height: 80%;
        overflow-y: auto;
      }
      .language-modal-header {
        display: flex;
        justify-content: space-between;
        align-items: center;
        margin-bottom: 20px;
      }
      .language-modal-close {
        background: none;
        border: none;
        font-size: 24px;
        cursor: pointer;
      }
      .language-grid {
        display: grid;
        grid-template-columns: repeat(auto-fill, minmax(150px, 1fr));
        gap: 10px;
      }
      .language-item {
        padding: 10px;
        border: 1px solid #ddd;
        border-radius: 4px;
        cursor: pointer;
        transition: background-color 0.3s;
      }
      .language-item:hover {
        background-color: #f5f5f5;
      }
      .language-item.active {
        background-color: #e6f7ff;
        border-color: #1890ff;
      }
    `;
    
    document.head.appendChild(style);
    document.body.appendChild(modal);
    
    // Заполняем сеткой языков
    const grid = modal.querySelector('.language-grid');
    
    allLanguages.forEach(lang => {
      const item = document.createElement('div');
      item.className = 'language-item';
      if (lang.code === currentLanguage) {
        item.classList.add('active');
      }
      item.innerHTML = `${lang.flag} ${lang.name}`;
      item.addEventListener('click', function() {
        changeLanguage(lang.code);
        document.body.removeChild(modal);
      });
      
      grid.appendChild(item);
    });
    
    // Обработчик закрытия
    modal.querySelector('.language-modal-close').addEventListener('click', function() {
      document.body.removeChild(modal);
    });
    
    // Закрытие по клику вне модального окна
    modal.addEventListener('click', function(e) {
      if (e.target === modal) {
        document.body.removeChild(modal);
      }
    });
  }
  
  /**
   * Изменить текущий язык
   * @param {string} lang - Код языка
   */
  function changeLanguage(lang) {
    if (lang === currentLanguage) return;
    
    // Сохраняем новый язык
    currentLanguage = lang;
    document.documentElement.lang = lang;
    
    // Для языков с письмом справа налево
    if (['ar', 'he'].includes(lang)) {
      document.documentElement.dir = 'rtl';
    } else {
      document.documentElement.dir = 'ltr';
    }
    
    // Обновляем cookie и localStorage
    setCookie('i18next', lang, 365);
    localStorage.setItem('i18nextLng', lang);
    
    // Обновляем переводы на странице
    updatePageTranslations();
    
    // Обновляем класс active у опций выбора языка
    const options = document.querySelectorAll('.language-option, .language-item');
    options.forEach(option => {
      if (option.dataset.lang === lang) {
        option.classList.add('active');
      } else {
        option.classList.remove('active');
      }
    });
    
    // Генерируем событие изменения языка
    document.dispatchEvent(new CustomEvent('languageChanged', { detail: { language: lang } }));
  }
  
  /**
   * Обновление переводов на странице
   */
  function updatePageTranslations() {
    // Если язык немецкий (базовый), не нужно ничего переводить
    if (currentLanguage === 'de') return;
    
    // Собираем все элементы с атрибутом data-i18n
    const elements = document.querySelectorAll('[data-i18n]');
    const textsToTranslate = [];
    
    // Собираем тексты для перевода
    elements.forEach(el => {
      const key = el.getAttribute('data-i18n');
      textsToTranslate.push(el.textContent.trim());
    });
    
    // Если нечего переводить, выходим
    if (textsToTranslate.length === 0) return;
    
    // Отправляем запрос на пакетный перевод
    fetch('/api/translate-batch', {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json'
      },
      body: JSON.stringify({
        texts: textsToTranslate,
        targetLang: currentLanguage
      })
    })
    .then(response => response.json())
    .then(data => {
      if (data.translations && data.translations.length === textsToTranslate.length) {
        // Применяем переводы
        elements.forEach((el, index) => {
          el.textContent = data.translations[index];
        });
      }
    })
    .catch(error => {
      console.error('Translation error:', error);
    });
    
    // Переводим атрибуты
    const attributeElements = document.querySelectorAll('[data-i18n-attr]');
    const attributeTexts = [];
    const attributeMappings = [];
    
    attributeElements.forEach(el => {
      try {
        const attrsMap = JSON.parse(el.getAttribute('data-i18n-attr'));
        for (const [attr, text] of Object.entries(attrsMap)) {
          attributeTexts.push(el.getAttribute(attr));
          attributeMappings.push({ element: el, attribute: attr });
        }
      } catch (e) {
        console.error('Error parsing data-i18n-attr:', e);
      }
    });
    
    // Если есть атрибуты для перевода
    if (attributeTexts.length > 0) {
      fetch('/api/translate-batch', {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json'
        },
        body: JSON.stringify({
          texts: attributeTexts,
          targetLang: currentLanguage
        })
      })
      .then(response => response.json())
      .then(data => {
        if (data.translations && data.translations.length === attributeTexts.length) {
          // Применяем переводы атрибутов
          attributeMappings.forEach((mapping, index) => {
            mapping.element.setAttribute(mapping.attribute, data.translations[index]);
          });
        }
      })
      .catch(error => {
        console.error('Attribute translation error:', error);
      });
    }
  }
  
  /**
   * Функция для перевода динамически созданного элемента
   * @param {HTMLElement} element - HTML-элемент для перевода
   * @param {string} context - Контекст перевода
   * @returns {Promise} - Promise с результатом перевода
   */
  function translateDynamicElement(element, context = '') {
    // Если язык немецкий (базовый), не нужно ничего переводить
    if (currentLanguage === 'de') return Promise.resolve();
    
    // Получаем все текстовые узлы этого элемента
    const textNodes = [];
    const walker = document.createTreeWalker(
      element,
      NodeFilter.SHOW_TEXT,
      null,
      false
    );
    
    let node;
    while (node = walker.nextNode()) {
      if (node.nodeValue.trim() !== '') {
        textNodes.push(node);
      }
    }
    
    // Атрибуты с текстом для перевода
    const attributesToTranslate = ['placeholder', 'title', 'value'];
    
    // Находим элементы для перевода
    const elementsWithAttributes = element.querySelectorAll(
      attributesToTranslate.map(attr => `[${attr}]`).join(',')
    );
    
    // Все текстовые значения для отправки на сервер
    const textsToTranslate = [];
    
    // Добавляем текстовые узлы
    textNodes.forEach(node => {
      textsToTranslate.push(node.nodeValue.trim());
    });
    
    // Добавляем значения атрибутов
    elementsWithAttributes.forEach(el => {
      attributesToTranslate.forEach(attr => {
        if (el.hasAttribute(attr) && el.getAttribute(attr).trim() !== '') {
          textsToTranslate.push(el.getAttribute(attr));
        }
      });
    });
    
    // Если нечего переводить, выходим
    if (textsToTranslate.length === 0) return Promise.resolve();
    
    // Отправляем запрос на перевод
    return fetch('/api/translate-batch', {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json'
      },
      body: JSON.stringify({
        texts: textsToTranslate,
        targetLang: currentLanguage,
        context: context
      })
    })
    .then(response => response.json())
    .then(data => {
      if (!data.translations || data.translations.length !== textsToTranslate.length) {
        throw new Error('Invalid translation response');
      }
      
      // Применяем переводы к текстовым узлам
      let index = 0;
      textNodes.forEach(node => {
        node.nodeValue = data.translations[index++];
      });
      
      // Применяем переводы к атрибутам
      elementsWithAttributes.forEach(el => {
        attributesToTranslate.forEach(attr => {
          if (el.hasAttribute(attr) && el.getAttribute(attr).trim() !== '') {
            el.setAttribute(attr, data.translations[index++]);
          }
        });
      });
      
      return data;
    })
    .catch(error => {
      console.error('Translation error:', error);
      return null;
    });
  }
  
  /**
   * Вспомогательная функция для установки cookie
   * @param {string} name - Имя cookie
   * @param {string} value - Значение cookie
   * @param {number} days - Количество дней до истечения срока действия
   */
  function setCookie(name, value, days) {
    let expires = '';
    if (days) {
      const date = new Date();
      date.setTime(date.getTime() + (days * 24 * 60 * 60 * 1000));
      expires = '; expires=' + date.toUTCString();
    }
    document.cookie = name + '=' + encodeURIComponent(value) + expires + '; path=/';
  }
  
  /**
   * Вспомогательная функция для получения cookie
   * @param {string} name - Имя cookie
   * @returns {string|null} - Значение cookie или null, если cookie не найден
   */
  function getCookie(name) {
    const nameEQ = name + '=';
    const ca = document.cookie.split(';');
    for (let i = 0; i < ca.length; i++) {
      let c = ca[i];
      while (c.charAt(0) === ' ') c = c.substring(1, c.length);
      if (c.indexOf(nameEQ) === 0) {
        return decodeURIComponent(c.substring(nameEQ.length, c.length));
      }
    }
    return null;
  }
  
  // Экспортируем функции в глобальное пространство имен
  window.i18n = {
    init,
    changeLanguage,
    getCurrentLanguage: () => currentLanguage,
    updatePageTranslations,
    translateDynamicElement
  };
  
  // Автоматическая инициализация при загрузке скрипта
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }
})();
