// i18n.js - Оптимизированный клиент для интернационализации

(function() {
  // Язык по умолчанию
  const defaultLanguage = 'en';
  
  // Кэш переводов (в памяти)
  const translationsCache = {};
  
  // Отслеживание ожидающих переводов
  const pendingTranslations = {};
  
  // Типы ожидающих элементов
  const PENDING_TYPES = {
    TEXT: 'text',
    ATTR: 'attribute',
    HTML: 'html'
  };
  
  // Флаг для режима совместимости с RMA
  let compatibilityMode = {
    enabled: true,
    defaultNamespace: 'rma'
  };
  
  // Состояние инициализации
  let isInitialized = false;
  
  // Режим совместимости с RMA
  let rmaCompatibilityMode = true;
  
  // Настройки логирования
  const VERBOSE_LOGGING = false;
  const LOG_ERRORS = true;
  
  /**
   * Расширенное логирование, которое можно легко включить/выключить
   */
  function log(...args) {
    if (VERBOSE_LOGGING) {
      console.log('[i18n]', ...args);
    }
  }
  
  /**
   * Логирование ошибок
   */
  function logError(...args) {
    if (LOG_ERRORS) {
      console.error('[i18n ERROR]', ...args);
    }
  }
  
  /**
   * Получает язык из мета-тега (предоставленного сервером)
   */
  function getLangFromMeta() {
    const metaTag = document.querySelector('meta[name="app-language"]');
    return metaTag ? metaTag.content : null;
  }
  
  /**
   * Получает текущий язык из различных источников
   */
  /**
  * Получает текущий язык из различных источников и НОРМАЛИЗУЕТ его
  */
 function getCurrentLanguage() {
  let detectedLang = null;
  let source = 'unknown';
  const sourcesChecked = []; // Для логирования

  // 1. Мета-тег
  const metaLang = getLangFromMeta();
  sourcesChecked.push(`Meta: ${metaLang}`);
  if (metaLang) {
      detectedLang = metaLang;
      source = 'meta';
  }

  // 2. Атрибут HTML lang
  if (!detectedLang) {
      const htmlLang = document.documentElement.lang;
      sourcesChecked.push(`HTML: ${htmlLang}`);
      if (htmlLang) {
          detectedLang = htmlLang;
          source = 'html';
      }
  }

  // 3. Cookie
  if (!detectedLang) {
      const cookieMatch = document.cookie.match(/i18next=([^;]+)/);
      const cookieLang = cookieMatch ? cookieMatch[1] : null;
      sourcesChecked.push(`Cookie: ${cookieLang}`);
      if (cookieLang) {
          detectedLang = cookieLang;
          source = 'cookie';
      }
  }
  
  // 4. LocalStorage
  if (!detectedLang) {
      try {
          const lsLang = localStorage.getItem('i18nextLng');
          sourcesChecked.push(`LocalStorage: ${lsLang}`);
          if (lsLang) {
              detectedLang = lsLang;
              source = 'localStorage';
          }
      } catch (e) { sourcesChecked.push(`LocalStorage: Error`); }
  }

  // 5. Язык по умолчанию
  if (!detectedLang) {
      detectedLang = defaultLanguage;
      source = 'default';
      sourcesChecked.push(`Default: ${detectedLang}`);
  }

  // --- НОРМАЛИЗАЦИЯ ---
  let normalizedLang = detectedLang;
  if (detectedLang && typeof detectedLang === 'string' && detectedLang.includes('-')) {
      normalizedLang = detectedLang.split('-')[0]; // Берем часть до дефиса
      console.log(`[i18n Client] Normalizing ${detectedLang} (from ${source}) to ${normalizedLang}. Sources checked: [${sourcesChecked.join(', ')}]`);
  } else {
      console.log(`[i18n Client] Using language ${normalizedLang} (from ${source}). Sources checked: [${sourcesChecked.join(', ')}]`);
  }
  // --- КОНЕЦ НОРМАЛИЗАЦИИ ---

  return normalizedLang; // Возвращаем ТОЛЬКО нормализованный язык
}

  /**
   * Инициализация i18n с приоритизацией мета-тега
   */
  function init() {
    // Если уже инициализирован, пропускаем
    if (isInitialized) {
      log('Уже инициализирован, пропускаем');
      return Promise.resolve();
    }
    
    // Отмечаем как инициализированный
    isInitialized = true;
    
    // Получаем язык из мета-тега (предоставленного сервером)
    const language = getCurrentLanguage();
    
    log(`Инициализация с языком: ${language}`);
    
    // Устанавливаем атрибуты языка
    document.documentElement.lang = language;
    
    // Устанавливаем направление RTL для RTL языков
    if (['ar', 'he'].includes(language)) {
      document.documentElement.dir = 'rtl';
    } else {
      document.documentElement.dir = 'ltr';
    }
    
    // Устанавливаем глобальный язык
    window.language = language;
    
    // Обновляем языковые маски в SVG
    syncLanguageMasks();
    
    // Настраиваем перехват fetch для заголовков языка
    setupFetchInterception();
    
    // Переводим существующие элементы
    if (language !== defaultLanguage) {
      log(`Язык не является языком по умолчанию (${defaultLanguage}), переводим элементы страницы`);
      translatePageElements();
    } else {
      log(`Язык по умолчанию (${defaultLanguage}), пропускаем перевод`);
    }
    
    // Настраиваем наблюдатель мутаций для динамического контента
    setupMutationObserver();
    
    // Совместимость с RMA: генерируем событие инициализации
    if (rmaCompatibilityMode) {
      log('Генерируем событие i18n:initialized для совместимости с RMA');
      setTimeout(() => {
        const initEvent = new CustomEvent('i18n:initialized', {
          detail: { language }
        });
        document.dispatchEvent(initEvent);
      }, 10);
    }
    
    log('Инициализация завершена');
    return Promise.resolve(language);
  }
  
  // Инициализация при готовности DOM
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', function() {
      if (window.translationUtils) {
        init();
      } else {
        // Ждем загрузки translationUtils
        const checkInterval = setInterval(function() {
          if (window.translationUtils) {
            clearInterval(checkInterval);
            init();
          }
        }, 50);
      }
    });
  } else {
    if (window.translationUtils) {
      init();
    } else {
      // Ждем загрузки translationUtils
      const checkInterval = setInterval(function() {
        if (window.translationUtils) {
          clearInterval(checkInterval);
          init();
        }
      }, 50);
    }
  }
  
  /**
   * Настраиваем перехват fetch для добавления языковых заголовков ко всем запросам
   */
  function setupFetchInterception() {
    log('Настраиваем перехват fetch для языковых заголовков');
    const originalFetch = window.fetch;
    window.fetch = function(url, options = {}) {
      if (!options.headers) {
        options.headers = {};
      }
      
      const currentLang = getCurrentLanguage();
      
      // Добавляем заголовок app-language ко всем fetch запросам
      if (typeof options.headers.set === 'function') {
        options.headers.set('app-language', currentLang);
      } else {
        options.headers['app-language'] = currentLang;
      }
      
      return originalFetch.call(this, url, options);
    };
  }
  
  /**
   * Настраиваем наблюдатель мутаций для перевода динамически добавленных элементов
   */
  function setupMutationObserver() {
    if (!window.MutationObserver) {
      log('MutationObserver not supported, skipping dynamic translation setup');
      return;
    }
    
    log('Setting up MutationObserver for dynamic content');
    const observer = new MutationObserver(mutations => {
      let elementsToTranslate = 0;
      
      for (const mutation of mutations) {
        if (mutation.type === 'childList' && mutation.addedNodes.length) {
          for (const node of mutation.addedNodes) {
            if (node.nodeType === Node.ELEMENT_NODE) {
              if (needsTranslation(node)) {
                elementsToTranslate++;
                // Используем новый метод для перевода
                translateElement(node);
              }
            }
          }
        }
      }
      
      if (elementsToTranslate > 0) {
        log(`Started translation for ${elementsToTranslate} dynamically added elements`);
      }
    });
    
    // Начинаем наблюдение
    observer.observe(document.body, {
      childList: true,
      subtree: true
    });
  }
  
  /**
   * Проверяем, нуждается ли элемент в переводе
   */
  function needsTranslation(element) {
    // Проверяем, имеет ли сам элемент атрибуты перевода
    if (element.hasAttribute('data-i18n') || 
        element.hasAttribute('data-i18n-attr') || 
        element.hasAttribute('data-i18n-html')) {
      return true;
    }
    
    // Проверяем, имеет ли какой-либо из дочерних элементов атрибуты перевода
    return element.querySelector('[data-i18n], [data-i18n-attr], [data-i18n-html]') !== null;
  }
  
  /**
   * Возможно добавить namespace к ключу, если его нет и включен режим совместимости
   * @param {string} key - Исходный ключ перевода
   * @returns {string} - Ключ с namespace при необходимости
   */
  function maybeAddNamespace(key) {
    if (compatibilityMode.enabled && !key.includes(':')) {
      return `${compatibilityMode.defaultNamespace}:${key}`;
    }
    return key;
  }
  
  /**
   * Загружает базовые и указанные пространства имен переводов
   * @param {string} language - Код языка
   * @param {string[]} extraNamespaces - Дополнительные пространства имен для загрузки
   */
  async function loadCommonNamespaces(language, extraNamespaces = []) {
    if (language === defaultLanguage) return;
    
    // Только базовый namespace по умолчанию
    const baseNamespaces = ['common'];
    
    // Объединяем с дополнительными namespaces, устраняя дубликаты
    const namespacesToLoad = [...new Set([...baseNamespaces, ...extraNamespaces])];
    
    log(`Loading namespaces for ${language}: ${namespacesToLoad.join(', ')}`);
    
    // Загружаем параллельно для ускорения
    const loadPromises = namespacesToLoad.map(namespace => 
      loadNamespaceFile(language, namespace)
    );
    
    await Promise.all(loadPromises);
    log(`Finished loading namespaces for ${language}`);
  }
  
  /**
   * Переводим элемент и его дочерние элементы
   */
  async function translateElement(element, options = {}) {
    const language = getCurrentLanguage();
    
    // Пропускаем перевод для языка по умолчанию
    if (language === defaultLanguage) {
      log('Using default language, skipping element translation');
      return;
    }
    
    // Определяем namespace для загрузки из опций или контекста
    const namespaceToUse = options.namespace || 
                          (compatibilityMode.enabled ? compatibilityMode.defaultNamespace : null);
    
    // Загружаем только необходимые namespaces
    const extraNamespaces = namespaceToUse ? [namespaceToUse] : [];
    await loadCommonNamespaces(language, extraNamespaces);
    
    log(`Translating element and its children for ${language}:`, element);
    
    // Обрабатываем стандартные переводы
    const standardElements = [element, ...Array.from(element.querySelectorAll('[data-i18n]'))];
    if (standardElements.length > 0) {
      log(`Found ${standardElements.length} elements with data-i18n`);
      
      for (const el of standardElements) {
        if (el.hasAttribute('data-i18n')) {
          await processElementWithLocalFiles(el);
        }
      }
    }
    
    // Обрабатываем переводы атрибутов
    const attrElements = [element, ...Array.from(element.querySelectorAll('[data-i18n-attr]'))];
    if (attrElements.length > 0) {
      log(`Found ${attrElements.length} elements with data-i18n-attr`);
      
      for (const el of attrElements) {
        if (el.hasAttribute('data-i18n-attr')) {
          await processAttrWithLocalFiles(el);
        }
      }
    }
    
    // Обрабатываем HTML переводы
    const htmlElements = [element, ...Array.from(element.querySelectorAll('[data-i18n-html]'))];
    if (htmlElements.length > 0) {
      log(`Found ${htmlElements.length} elements with data-i18n-html`);
      
      for (const el of htmlElements) {
        if (el.hasAttribute('data-i18n-html')) {
          await processHtmlWithLocalFiles(el);
        }
      }
    }
    
    log('Element and children translation completed');
  }
  
  /**
   * Обрабатываем элемент с переводом текста с поддержкой локальных файлов
   * @param {HTMLElement} element - Элемент для перевода
   */
  async function processElementWithLocalFiles(element) {
    const key = element.getAttribute('data-i18n');
    if (!key) return;
    
    // Получаем оригинальный текст
    const originalText = element.textContent.trim();
    
    // Обрабатываем опции
    let options = {};
    try {
      const optionsAttr = element.getAttribute('data-i18n-options');
      if (optionsAttr) {
        options = JSON.parse(optionsAttr);
      }
    } catch (error) {
      logError(`Error parsing data-i18n-options for ${key}:`, error);
    }
    
    // Поддерживаем оба стиля префиксации - важное исправление!
    const adjustedKey = maybeAddNamespace(key);
    
    // Сначала пробуем получить перевод из локальных файлов
    const localTranslation = await getLocalTranslation(adjustedKey, options);
    
    if (localTranslation) {
      // Применяем перевод из файла
      element.textContent = localTranslation;
      
      // Удаляем атрибуты перевода
      element.removeAttribute('data-i18n');
      if (element.hasAttribute('data-i18n-options')) {
        element.removeAttribute('data-i18n-options');
      }
      
      log(`Applied local file translation for "${adjustedKey}": "${localTranslation.substring(0, 30)}${localTranslation.length > 30 ? '...' : ''}"`);
      return;
    }
    
    // Если не нашли в локальных файлах, используем API перевод
    try {
      await processElementTranslation(element);
    } catch (error) {
      logError(`Failed to translate element with key "${adjustedKey}":`, error);
    }
  }
  
  /**
   * Обрабатываем элемент с переводом атрибутов с поддержкой локальных файлов
   * @param {HTMLElement} element - Элемент для перевода
   */
  async function processAttrWithLocalFiles(element) {
    try {
      const attrsJson = element.getAttribute('data-i18n-attr');
      if (!attrsJson) return;
      
      const attrs = JSON.parse(attrsJson);
      log(`Processing attributes with local files: ${attrsJson}`, element);
      
      // Отслеживаем успешные переводы
      let successfulTranslations = 0;
      const totalAttributes = Object.keys(attrs).length;
      
      // Обрабатываем каждый атрибут
      for (const [attr, key] of Object.entries(attrs)) {
        // Получаем текущее значение атрибута
        const originalValue = element.getAttribute(attr) || '';
        
        // Обрабатываем опции
        let options = {};
        try {
          const optionsAttr = element.getAttribute('data-i18n-options');
          if (optionsAttr) {
            options = JSON.parse(optionsAttr);
          }
        } catch (error) {
          logError(`Error parsing data-i18n-options for attribute "${attr}":`, error);
        }
        
        // Поддерживаем оба стиля префиксации
        const adjustedKey = maybeAddNamespace(key);
        
        // Сначала пробуем получить перевод из локальных файлов
        const localTranslation = await getLocalTranslation(adjustedKey, options);
        
        if (localTranslation) {
          // Применяем перевод из файла
          element.setAttribute(attr, localTranslation);
          successfulTranslations++;
          
          log(`Applied local file translation for attribute "${attr}" with key "${adjustedKey}": "${localTranslation}"`);
        } else {
          // Если не нашли в локальных файлах, используем перевод через API
          try {
            // Информация об элементе для добавления в список ожидания
            const pendingInfo = {
              element: element,
              type: PENDING_TYPES.ATTR,
              attr: attr,
              key: adjustedKey
            };
            
            const response = await fetchTranslation(adjustedKey, originalValue, options, pendingInfo);
            
            if (response) {
              // Извлекаем перевод и статус
              const translation = typeof response === 'object' ? response.translated : response;
              const status = typeof response === 'object' ? response.status : 'complete';
              const fromSource = typeof response === 'object' ? response.fromSource : false;
              
              if (translation) {
                element.setAttribute(attr, translation);
                log(`Applied API translation for attribute "${attr}": "${translation}"`);
                
                // Считаем успешным только если статус complete и не из исходного языка
                if (status === 'complete' && !fromSource) {
                  successfulTranslations++;
                }
              }
            }
          } catch (error) {
            logError(`Error translating attribute "${attr}" with key "${adjustedKey}":`, error);
          }
        }
      }
      
      // Удаляем data-i18n-attr после успешного перевода всех атрибутов
      if (successfulTranslations === totalAttributes) {
        element.removeAttribute('data-i18n-attr');
        
        if (element.hasAttribute('data-i18n-options')) {
          element.removeAttribute('data-i18n-options');
        }
        
        log(`Removed data-i18n-attr after successful translation of all ${totalAttributes} attributes`);
      } else {
        log(`Keeping data-i18n-attr, only ${successfulTranslations}/${totalAttributes} attributes translated successfully`);
      }
    } catch (error) {
      logError('Error processing attribute translation with local files:', error);
    }
  }
  
  /**
   * Обрабатываем элемент с переводом HTML с поддержкой локальных файлов
   * @param {HTMLElement} element - Элемент для перевода
   */
  async function processHtmlWithLocalFiles(element) {
    const key = element.getAttribute('data-i18n-html');
    if (!key) return;
    
    // Получаем оригинальный HTML
    const originalHtml = element.innerHTML.trim();
    
    // Обрабатываем опции
    let options = {};
    try {
      const optionsAttr = element.getAttribute('data-i18n-options');
      if (optionsAttr) {
        options = JSON.parse(optionsAttr);
      }
    } catch (error) {
      logError(`Error parsing data-i18n-options for HTML key "${key}":`, error);
    }
    
    // Поддерживаем оба стиля префиксации
    const adjustedKey = maybeAddNamespace(key);
    
    // Сначала пробуем получить перевод из локальных файлов
    const localTranslation = await getLocalTranslation(adjustedKey, options);
    
    if (localTranslation) {
      // Применяем перевод из файла
      element.innerHTML = localTranslation;
      
      // Удаляем атрибуты перевода
      element.removeAttribute('data-i18n-html');
      if (element.hasAttribute('data-i18n-options')) {
        element.removeAttribute('data-i18n-options');
      }
      
      log(`Applied local file translation for HTML "${adjustedKey}": "${localTranslation.substring(0, 30)}${localTranslation.length > 30 ? '...' : ''}"`);
      return;
    }
    
    // Если не нашли в локальных файлах, используем API перевод
    try {
      await processHtmlTranslation(element);
    } catch (error) {
      logError(`Failed to translate HTML element with key "${adjustedKey}":`, error);
    }
  }
  
  /**
   * Переводим все элементы страницы
   */
  function translatePageElements() {
    const language = getCurrentLanguage();
    log(`Начинаем перевод страницы для языка: ${language}`);
    
    // Пропускаем, если язык по умолчанию
    if (language === defaultLanguage) {
      log('Используем язык по умолчанию, пропускаем перевод');
      return;
    }
    
    // Обрабатываем стандартные переводы
    const standardElements = document.querySelectorAll('[data-i18n]');
    log(`Найдено ${standardElements.length} элементов с data-i18n`);
    standardElements.forEach(processElementTranslation);
    
    // Обрабатываем переводы атрибутов
    const attrElements = document.querySelectorAll('[data-i18n-attr]');
    log(`Найдено ${attrElements.length} элементов с data-i18n-attr`);
    attrElements.forEach(processAttrTranslation);
    
    // Обрабатываем HTML переводы
    const htmlElements = document.querySelectorAll('[data-i18n-html]');
    log(`Найдено ${htmlElements.length} элементов с data-i18n-html`);
    htmlElements.forEach(processHtmlTranslation);
    
    log('Перевод страницы запущен, ожидаем ответов API');
  }


  // Добавьте в i18n.js после объявления translateElement:

/**
 * Alias для translateElement для обратной совместимости
 * @param {HTMLElement} element - Элемент для перевода
 * @param {Object} options - Дополнительные опции
 */
function translateDynamicElement(element, options = {}) {
  return translateElement(element, options);
}
  
  /**
   * Обрабатываем перевод текста элемента с удалением атрибута после подтвержденного перевода
   */
  function processElementTranslation(element) {
    const key = element.getAttribute('data-i18n');
    if (!key) return;
    
    // Поддерживаем оба стиля префиксации
    const adjustedKey = maybeAddNamespace(key);
    
    // Получаем оригинальный текст для API перевода
    const originalText = element.textContent.trim();
    
    // Обрабатываем дополнительные опции
    let options = {};
    try {
      const optionsAttr = element.getAttribute('data-i18n-options');
      if (optionsAttr) {
        options = JSON.parse(optionsAttr);
        log(`Обрабатываем элемент с опциями: ${optionsAttr}`, element);
      }
    } catch (error) {
      logError('Ошибка при разборе data-i18n-options:', error);
    }
    
    // Добавляем defaultValue в опции
    options.defaultValue = originalText;
    
    log(`Переводим элемент с ключом: "${adjustedKey}", текст: "${originalText.substring(0, 30)}${originalText.length > 30 ? '...' : ''}"`);
    
    // Информация об элементе для добавления в список ожидания
    const pendingInfo = {
      element: element,
      type: PENDING_TYPES.TEXT,
      key: adjustedKey
    };
    
    // Сначала проверяем, существует ли перевод на основе файла - это приоритетнее API
    checkTranslationFile(adjustedKey, options).then(fileTranslation => {
      if (fileTranslation) {
        log(`Применен перевод из файла для "${adjustedKey}": "${fileTranslation}"`);
        element.textContent = fileTranslation;
        
        // Переводы на основе файлов всегда считаются успешными,
        // потому что они приходят из доверенных файлов перевода
        element.removeAttribute('data-i18n');
        
        // Также удаляем опции, если они присутствуют
        if (element.hasAttribute('data-i18n-options')) {
          element.removeAttribute('data-i18n-options');
        }
        
        log(`Удален атрибут data-i18n после успешного перевода из файла`);
        return;
      }
      
      // Нет перевода из файла, пробуем API
      fetchTranslation(adjustedKey, originalText, options, pendingInfo).then(response => {
        // Ответ теперь должен включать как перевод, так и статус
        // Если нет ответа, сохраняем атрибут data-i18n
        if (!response) {
          log(`Нет ответа от API для "${adjustedKey}", сохраняем атрибут data-i18n`);
          return;
        }
        
        // Извлекаем перевод и статус
        const translation = typeof response === 'object' ? response.translated : response;
        const status = typeof response === 'object' ? response.status : 'complete';
        const fromCache = typeof response === 'object' ? response.fromCache : false;
        const fromSource = typeof response === 'object' ? response.fromSource : false;
        
        // Устанавливаем перевод независимо от статуса
        if (translation) {
          element.textContent = translation;
          log(`Применен перевод API для "${adjustedKey}": "${translation.substring(0, 30)}${translation.length > 30 ? '...' : ''}"`);
        }
        
        // Удаляем теги i18n только если перевод был действительно успешным
        // Это означает:
        // 1. Статус 'complete'
        // 2. Мы получили перевод
        // 3. НЕ флаг 'fromSource' (который означает, что мы используем исходный текст, потому что мы в исходном языке)
        if (status === 'complete' && translation && !fromSource) {
          element.removeAttribute('data-i18n');
          
          // Также удаляем опции, если они присутствуют
          if (element.hasAttribute('data-i18n-options')) {
            element.removeAttribute('data-i18n-options');
          }
          
          log(`Удален атрибут data-i18n после подтвержденного успешного перевода API`);
        } else {
          log(`Сохраняем атрибут data-i18n, потому что статус перевода: ${status}, fromSource: ${fromSource}`);
        }
      }).catch(err => {
        logError(`Не удалось получить перевод для ключа "${adjustedKey}":`, err);
        // При ошибке мы определенно хотим сохранить атрибут data-i18n
      });
    }).catch(err => {
      logError(`Ошибка при проверке перевода из файла для "${adjustedKey}":`, err);
      
      // Возвращаемся к API переводу при ошибке
      fetchTranslation(adjustedKey, originalText, options, pendingInfo).then(response => {
        // То же самое, что и выше
        if (!response) {
          log(`Нет ответа от API для "${adjustedKey}" после ошибки файла, сохраняем атрибут data-i18n`);
          return;
        }
        
        const translation = typeof response === 'object' ? response.translated : response;
        const status = typeof response === 'object' ? response.status : 'complete';
        const fromSource = typeof response === 'object' ? response.fromSource : false;
        
        if (translation) {
          element.textContent = translation;
        }
        
        if (status === 'complete' && translation && !fromSource) {
          element.removeAttribute('data-i18n');
          
          if (element.hasAttribute('data-i18n-options')) {
            element.removeAttribute('data-i18n-options');
          }
          
          log(`Удален атрибут data-i18n после подтвержденного перевода API (запасной путь)`);
        } else {
          log(`Сохраняем атрибут data-i18n в запасном пути, статус: ${status}, fromSource: ${fromSource}`);
        }
      }).catch(() => {
        // Тихий перехват - уже залогировано в fetchTranslation
        // Сохраняем атрибут data-i18n при ошибке
      });
    });
  }

  /**
   * Обрабатываем перевод атрибутов с удалением атрибута после подтвержденного перевода
   * @param {HTMLElement} element - Элемент с data-i18n-attr
   */
  function processAttrTranslation(element) {
    try {
      const attrsJson = element.getAttribute('data-i18n-attr');
      if (!attrsJson) return;
      
      const attrs = JSON.parse(attrsJson);
      log(`Processing attribute translations: ${attrsJson}`, element);
      
      // Track translation promises for each attribute
      const translationPromises = [];
      
      for (const [attr, key] of Object.entries(attrs)) {
        // Поддерживаем оба стиля префиксации
        const adjustedKey = maybeAddNamespace(key);
        
        // Get original attribute value
        const originalValue = element.getAttribute(attr) || '';
        
        log(`Translating attribute "${attr}" with key "${adjustedKey}", value: "${originalValue}"`);
        
        // Pending info for tracking
        const pendingInfo = {
          element: element,
          type: PENDING_TYPES.ATTR,
          attr: attr,
          key: adjustedKey
        };
        
        // Create Promise for this attribute's translation
        const translationPromise = (async () => {
          // First check for file-based translation
          try {
            const fileTranslation = await checkTranslationFile(adjustedKey, {});
            if (fileTranslation) {
              log(`Applied file translation for attribute "${attr}": "${fileTranslation}"`);
              element.setAttribute(attr, fileTranslation);
              // File translations are always considered successful
              return true;
            }
            
            // No file translation, try API
            const response = await fetchTranslation(adjustedKey, originalValue, {}, pendingInfo);
            if (!response) {
              log(`No response from API for attribute "${attr}", keeping attribute`);
              return false;
            }
            
            // Extract translation and status
            const translation = typeof response === 'object' ? response.translated : response;
            const status = typeof response === 'object' ? response.status : 'complete';
            const fromSource = typeof response === 'object' ? response.fromSource : false;
            
            if (translation) {
              element.setAttribute(attr, translation);
              log(`Applied API translation for attribute "${attr}": "${translation}"`);
            }
            
            // Only successful if we have translation, status is complete, and not from source language
            return (status === 'complete' && translation && !fromSource);
          } catch (err) {
            logError(`Error translating attribute "${attr}" with key "${adjustedKey}":`, err);
            return false; // Not successful
          }
        })();
        
        translationPromises.push(translationPromise);
      }
      
      // After all attribute translations complete, remove data-i18n-attr if all were successful
      Promise.all(translationPromises).then(results => {
        const allSuccessful = results.every(result => result === true);
        if (allSuccessful) {
          // Remove data-i18n-attr after successful translation of all attributes
          element.removeAttribute('data-i18n-attr');
          
          // Also remove options if present
          if (element.hasAttribute('data-i18n-options')) {
            element.removeAttribute('data-i18n-options');
          }
          
          log(`Removed data-i18n-attr after successful translation of all attributes`);
        } else {
          log(`Keeping data-i18n-attr because not all attributes were successfully translated`);
        }
      });
    } catch (error) {
      logError('Error processing attribute translation:', error);
    }
  }

  /**
   * Обрабатываем перевод HTML контента с удалением атрибута после подтвержденного перевода
   */
  function processHtmlTranslation(element) {
    const key = element.getAttribute('data-i18n-html');
    if (!key) return;
    
    // Поддерживаем оба стиля префиксации
    const adjustedKey = maybeAddNamespace(key);
    
    const originalHtml = element.innerHTML.trim();
    
    log(`Переводим HTML контент с ключом: "${adjustedKey}", длина: ${originalHtml.length} символов`);
    
    // Информация об элементе для добавления в список ожидания
    const pendingInfo = {
      element: element,
      type: PENDING_TYPES.HTML,
      key: adjustedKey
    };
    
    // Сначала проверяем, существует ли перевод на основе файла
    checkTranslationFile(adjustedKey, {}).then(fileTranslation => {
      if (fileTranslation) {
        log(`Применен HTML перевод из файла для "${adjustedKey}", длина: ${fileTranslation.length} символов`);
        element.innerHTML = fileTranslation;
        
        // Переводы на основе файлов всегда успешны
        element.removeAttribute('data-i18n-html');
        
        // Также удаляем опции, если они присутствуют
        if (element.hasAttribute('data-i18n-options')) {
          element.removeAttribute('data-i18n-options');
        }
        
        log(`Удален атрибут data-i18n-html после перевода из файла`);
        return;
      }
      
      // Нет перевода из файла, пробуем API
      fetchTranslation(adjustedKey, originalHtml, {}, pendingInfo).then(response => {
        // Обрабатываем ответ
        if (!response) {
          log(`Нет ответа от API для HTML "${adjustedKey}", сохраняем атрибут`);
          return;
        }
        
        // Извлекаем перевод и статус
        const translation = typeof response === 'object' ? response.translated : response;
        const status = typeof response === 'object' ? response.status : 'complete';
        const fromSource = typeof response === 'object' ? response.fromSource : false;
        
        if (translation) {
          element.innerHTML = translation;
          log(`Применен HTML перевод API для "${adjustedKey}", длина: ${translation.length} символов`);
        }
        
        // Удаляем теги i18n только если перевод был полностью успешным
        if (status === 'complete' && translation && !fromSource) {
          element.removeAttribute('data-i18n-html');
          
          // Также удаляем опции, если они присутствуют
          if (element.hasAttribute('data-i18n-options')) {
            element.removeAttribute('data-i18n-options');
          }
          
          log(`Удален атрибут data-i18n-html после подтвержденного перевода API`);
        } else {
          log(`Сохраняем атрибут data-i18n-html, потому что статус: ${status}, fromSource: ${fromSource}`);
        }
      }).catch(err => {
        logError(`Не удалось получить HTML перевод для ключа "${adjustedKey}":`, err);
        // При ошибке сохраняем атрибут data-i18n-html
      });
    }).catch(err => {
      logError(`Ошибка при проверке перевода из файла для HTML "${adjustedKey}":`, err);
      
      // Возвращаемся к API переводу при ошибке
      fetchTranslation(adjustedKey, originalHtml, {}, pendingInfo).then(response => {
        // То же самое, что и выше
        if (!response) {
          return;
        }
        
        const translation = typeof response === 'object' ? response.translated : response;
        const status = typeof response === 'object' ? response.status : 'complete';
        const fromSource = typeof response === 'object' ? response.fromSource : false;
        
        if (translation) {
          element.innerHTML = translation;
        }
        
        if (status === 'complete' && translation && !fromSource) {
          element.removeAttribute('data-i18n-html');
          
          if (element.hasAttribute('data-i18n-options')) {
            element.removeAttribute('data-i18n-options');
          }
        }
      }).catch(() => {
        // Тихий перехват - уже залогировано в fetchTranslation
        // Сохраняем атрибут data-i18n-html при ошибке
      });
    });
  }
  
  /**
   * Проверяем, существует ли перевод в файле
   * @param {string} key - Ключ перевода
   * @param {Object} options - Опции, такие как count и т.д.
   * @returns {Promise<string|null>} - Перевод или null, если не найден
   */
  async function checkTranslationFile(key, options = {}) {
    const language = getCurrentLanguage();
    
    // Пропускаем для языка по умолчанию
    if (language === defaultLanguage) {
      return null;
    }
    
    // Извлекаем пространство имен из ключа
    let namespace = 'common';
    let translationKey = key;
    
    if (key.includes(':')) {
      const parts = key.split(':');
      namespace = parts[0];
      translationKey = parts.slice(1).join(':');
    }
    
    // Проверяем, известно ли, что этот файл отсутствует
    if (window.translationFileCache && 
        window.translationFileCache.isFileMissing(`${language}:${namespace}`)) {
      return null;
    }
    
    // Пытаемся получить файл перевода с HEAD запросом сначала, чтобы избежать ошибок 404 в консоли
    try {
      const checkResponse = await fetch(`/locales/${language}/${namespace}.json`, {
        method: 'HEAD',
        cache: 'no-cache'
      });
      
      // Если файл не существует, отмечаем его и возвращаем null
      if (!checkResponse.ok) {
        if (window.translationFileCache) {
          window.translationFileCache.markAsMissing(`${language}:${namespace}`);
        }
        return null;
      }
      
      // Файл существует, теперь загружаем его
      const response = await fetch(`/locales/${language}/${namespace}.json`);
      if (!response.ok) {
        if (window.translationFileCache) {
          window.translationFileCache.markAsMissing(`${language}:${namespace}`);
        }
        return null;
      }
      
      // Разбираем файл
      const data = await response.json();
      
      // Переходим к ключу
      const keyParts = translationKey.split('.');
      let current = data;
      
      for (const part of keyParts) {
        if (!current || current[part] === undefined) {
          return null;
        }
        current = current[part];
      }
      
      // Если мы нашли перевод, применяем любые опции, такие как count
      if (current && typeof current === 'string') {
        let result = current;
        
        // Применяем замену count, если необходимо
        if (options.count !== undefined && result.includes('{{count}}')) {
          result = result.replace(/\{\{count\}\}/g, options.count);
        }
        
        return result;
      }
      
      return null;
    } catch (error) {
      // Отмечаем файл как отсутствующий, чтобы избежать повторных сбоев
      if (window.translationFileCache) {
        window.translationFileCache.markAsMissing(`${language}:${namespace}`);
      }
      
      // Повторно вызываем для обработки выше
      throw error;
    }
  }
  
  /**
   * Загружает файл перевода для указанного языка и пространства имен
   * @param {string} language - Код языка
   * @param {string} namespace - Пространство имен
   * @returns {Object|null} - Объект с переводами или null
   */
  async function loadNamespaceFile(language, namespace) {
    try {
      if (language === defaultLanguage) return null;
      
      console.log(`[i18n] Attempting to load namespace ${namespace} for language ${language}`);
      
      // Проверяем, не загружен ли уже файл
      if (window.i18nLoadedFiles && window.i18nLoadedFiles[`${language}:${namespace}`]) {
        return window.i18nLoadedFiles[`${language}:${namespace}`];
      }
      
      // Инициализируем объект для отслеживания загруженных файлов
      if (!window.i18nLoadedFiles) {
        window.i18nLoadedFiles = {};
      }
      
      // Проверяем, не является ли файл отсутствующим
      if (window.translationFileCache && window.translationFileCache.isFileMissing(`${language}:${namespace}`)) {
        log(`File ${language}:${namespace} is known to be missing, skipping`);
        return null;
      }
      
      // Сначала проверяем наличие файла с помощью HEAD запроса
      try {
        const checkResponse = await fetch(`/locales/${language}/${namespace}.json`, {
          method: 'HEAD',
          cache: 'no-cache'
        });
        
        if (checkResponse.ok) {
          // Файл существует, загружаем
          const response = await fetch(`/locales/${language}/${namespace}.json`);
          const data = await response.json();
          
          // Преобразуем в плоскую структуру для удобства поиска
          const flattenedData = flattenTranslations(data);
          
          // Сохраняем в глобальном кэше
          window.i18nLoadedFiles[`${language}:${namespace}`] = flattenedData;
          
          log(`Loaded translation file for ${language}:${namespace}`);
          return flattenedData;
        } else {
          // Файл не существует, помечаем
          if (window.translationFileCache) {
            window.translationFileCache.markAsMissing(`${language}:${namespace}`);
          }
          log(`Translation file not found for ${language}:${namespace}`);
          return null;
        }
      } catch (error) {
        log(`Error checking translation file for ${language}:${namespace}:`, error);
        if (window.translationFileCache) {
          window.translationFileCache.markAsMissing(`${language}:${namespace}`);
        }
        return null;
      }
    } catch (error) {
      log(`Failed to load namespace file ${language}:${namespace}:`, error);
      return null;
    }
  }
  
  /**
   * Ищет перевод для ключа в локальных файлах
   * @param {string} key - Ключ перевода
   * @param {Object} options - Опции перевода
   * @returns {Promise<string|null>} - Найденный перевод или null
   */
  async function getLocalTranslation(key, options = {}) {
    const language = getCurrentLanguage();
    
    if (language === defaultLanguage) return null;
    
    console.log(`[i18n] Searching for translation key ${key} in local files`);
    
    // Извлекаем пространство имен и ключ
    let namespace = 'common';
    let translationKey = key;
    
    if (key.includes(':')) {
      const parts = key.split(':');
      namespace = parts[0];
      translationKey = parts.slice(1).join(':');
    }
    
    console.log(`[i18n] Namespace: ${namespace}, Key: ${translationKey}`);
    
    // Загружаем файл перевода, если еще не загружен
    const translations = await loadNamespaceFile(language, namespace);
    
    if (!translations) {
      console.log(`[i18n] No translations found for namespace ${namespace}`);
      return null;
    }
    
    // Ищем перевод в плоской структуре
    if (translations[translationKey]) {
      let translated = translations[translationKey];
      console.log(`[i18n] Found translation in file: ${translated}`);
      
      // Обрабатываем шаблоны
      if (options.count !== undefined && translated.includes('{{count}}')) {
        translated = translated.replace(/\{\{count\}\}/g, options.count);
      }
      
      return translated;
    }
    
    console.log(`[i18n] Translation key ${translationKey} not found in namespace ${namespace}`);
    return null;
  }
  
  /**
   * Преобразует вложенную структуру переводов в плоскую с нотацией через точку
   * @param {Object} obj - Исходный объект переводов
   * @param {string} prefix - Префикс для ключей
   * @returns {Object} - Сглаженный объект
   */
  function flattenTranslations(obj, prefix = '') {
    return Object.keys(obj).reduce((acc, key) => {
      const prefixedKey = prefix ? `${prefix}.${key}` : key;
      
      if (typeof obj[key] === 'object' && obj[key] !== null) {
        Object.assign(acc, flattenTranslations(obj[key], prefixedKey));
      } else {
        acc[prefixedKey] = obj[key];
      }
      
      return acc;
    }, {});
  }
  
/**
 * Fetch translation with improved response handling and attribute support
 * @param {string} key - Translation key
 * @param {string} defaultText - Default text if translation fails
 * @param {Object} options - Translation options (count etc.)
 * @param {Object} pendingInfo - Information about the element being translated
 * @returns {Promise<Object|string>} - Response object with status or just translated text
 */
async function fetchTranslation(key, defaultText, options = {}, pendingInfo = null) {
  const language = getCurrentLanguage();
  
  // Skip translation for default language
  if (language === defaultLanguage) {
    log(`Using default language (${defaultLanguage}), skipping translation request for "${key}"`);
    return {
      translated: defaultText,
      original: defaultText,
      language: language,
      fromSource: true, // Important flag indicating this is from source language
      status: 'complete'
    };
  }
  
  // Extract namespace from key, if present
  let namespace = 'common';
  let translationKey = key;
  
  if (key.includes(':')) {
    const parts = key.split(':');
    namespace = parts[0];
    translationKey = parts.slice(1).join(':');
  }
  
  // Handle attribute translations - ensure defaultText is always provided
  if (pendingInfo && pendingInfo.type === PENDING_TYPES.ATTR) {
    // For attribute translations, try to get a better default value if none provided
    if (!defaultText || defaultText === 'null' || defaultText === '') {
      // Get fallback from the attribute
      const attrValue = pendingInfo.element.getAttribute(pendingInfo.attr);
      
      // If the element has the attribute, use it
      if (attrValue && attrValue !== 'null' && attrValue !== '') {
        defaultText = attrValue;
      } else {
        // Last resort - use the key itself as a fallback
        defaultText = translationKey.split('.').pop();
        
        // Add special handling for common attribute types
        if (pendingInfo.attr === 'placeholder') {
          if (translationKey.includes('serial')) {
            defaultText = 'Enter the serial number';
          } else if (translationKey.includes('description')) {
            defaultText = 'Describe the problem with the device';
          } else {
            defaultText = 'Enter value';
          }
        }
      }
      
      log(`Using fallback default text for attribute ${pendingInfo.attr}: "${defaultText}"`);
    }
  }
  
  // Use same algorithm as server for cache key generation
  const cacheKey = window.translationUtils ? 
      window.translationUtils.generateTranslationKey(defaultText, language, namespace, options) :
      `${language}:${namespace}:${translationKey}`;
  
  // Add language prefix for client cache
  const fullCacheKey = `${language}:${cacheKey}`;
  
  log(`Requesting translation for key: "${key}" with cache key: "${fullCacheKey}"`);
  
  // First check client cache
  if (translationsCache[fullCacheKey]) {
    log(`Found in client cache: "${fullCacheKey}"`);
    return {
      translated: translationsCache[fullCacheKey],
      original: defaultText,
      language: language,
      fromCache: true,
      status: 'complete'
    };
  }
  
  // Add to existing pending request if it exists
  if (pendingTranslations[fullCacheKey]) {
    log(`Translation already pending for "${fullCacheKey}", registering element for later update`);
    
    // If element info provided, register it for later update
    if (pendingInfo) {
      // Create elements array if it doesn't exist
      if (!pendingTranslations[fullCacheKey].elements) {
        pendingTranslations[fullCacheKey].elements = [];
      }
      pendingTranslations[fullCacheKey].elements.push(pendingInfo);
    }
    
    // Return existing Promise to avoid duplicate requests
    return pendingTranslations[fullCacheKey].request;
  }
  
  // Create new Promise for the request
  const translationPromise = new Promise(async (resolve, reject) => {
    try {
      log(`Making API request for translation: "${key}" in language: ${language}`);
      
      // Now try the API
      try {
        // Create properly formatted request payload - ensuring all required fields
        const payload = {
          text: defaultText,
          targetLang: language,
          context: namespace,
          key: translationKey,
          defaultValue: defaultText, // Must be included
          background: false,
          options: options
        };
        
        const response = await fetch(`/api/translate`, {
          method: 'POST',
          headers: {
            'Content-Type': 'application/json',
            'app-language': language
          },
          body: JSON.stringify(payload)
        });
        
        const data = await response.json();
        
        log(`API response for "${key}": status=${response.status}, fromCache=${data.fromCache}, translated length=${data.translated?.length}`);
        
        // Handle "pending" status with retry
        if (response.status === 202 && data.status === 'pending') {
          const retryAfter = parseInt(response.headers.get('Retry-After') || '3', 10);
          
          log(`Translation pending for "${key}", will retry in ${retryAfter} seconds`);
          
          // Schedule retry with server-suggested delay
          setTimeout(() => {
            log(`Retrying translation for "${key}" after delay`);
            
            // Save pending elements for later update
            const pendingElements = pendingTranslations[fullCacheKey]?.elements || [];
            
            // Remove from pending to allow new request
            delete pendingTranslations[fullCacheKey];
            
            // Request translation again
            fetchTranslation(key, defaultText, options).then(translation => {
              log(`Retry successful for "${key}", updating elements`);
              
              // Update all pending elements
              updateAllPendingElements(pendingElements, translation);
              
              // Resolve Promise with obtained translation
              resolve(translation);
            }).catch(err => {
              logError(`Retry failed for "${key}":`, err);
              reject(err);
            });
          }, retryAfter * 1000);
          
          // Return response object with pending status
          return {
            translated: defaultText, // Return original as placeholder
            original: defaultText,
            language: language,
            status: 'pending',
            retryAfter: retryAfter
          };
        }
        
        // Process completed translation
        if (data.translated) {
          const translation = data.translated;
          log(`Got translation for "${key}": "${translation.substring(0, 30)}${translation.length > 30 ? '...' : ''}"`);
          
          // Prepare response object
          const responseObj = {
            translated: translation,
            original: defaultText,
            language: language,
            fromCache: !!data.fromCache,
            fromSource: !!data.fromSource,
            status: data.status || 'complete'
          };
          
          // Cache translation using same key format as backend
          translationsCache[fullCacheKey] = translation;
          
          // Update all pending elements
          if (pendingTranslations[fullCacheKey] && pendingTranslations[fullCacheKey].elements) {
            updateAllPendingElements(pendingTranslations[fullCacheKey].elements, responseObj);
          }
          
          // Clear pending status
          delete pendingTranslations[fullCacheKey];
          
          // Also update all elements with this key
          updateAllElementsWithKey(key, responseObj);
          
          // Resolve Promise with response object
          resolve(responseObj);
          return responseObj;
        }
        
        // If we reach here, no usable translation
        log(`No usable translation for "${key}", using original text`);
        delete pendingTranslations[fullCacheKey];
        
        // Return response object with error status
        const errorResponseObj = {
          translated: defaultText,
          original: defaultText,
          language: language,
          status: 'error',
          error: 'Translation data not returned'
        };
        
        resolve(errorResponseObj);
        return errorResponseObj;
      } catch (error) {
        logError(`API error translating "${key}":`, error);
        delete pendingTranslations[fullCacheKey];
        
        // Return response object with error
        const errorResponseObj = {
          translated: defaultText,
          original: defaultText,
          language: language,
          status: 'error',
          error: error.message
        };
        
        resolve(errorResponseObj);
        return errorResponseObj;
      }
    } catch (error) {
      logError(`Unexpected error for "${key}":`, error);
      
      // Return response object with error
      const errorResponseObj = {
        translated: defaultText,
        original: defaultText,
        language: language,
        status: 'error',
        error: 'Unexpected error'
      };
      
      resolve(errorResponseObj);
      return errorResponseObj;
    }
  });
  
  // Save Promise and element info in pending list
  pendingTranslations[fullCacheKey] = {
    request: translationPromise,
    elements: pendingInfo ? [pendingInfo] : []
  };
  
  return translationPromise;
}
    
  /**
   * Update all pending elements waiting for translation
   * with attribute removal after confirmed translation
   */
  function updateAllPendingElements(pendingElements, response) {
    if (!pendingElements || !pendingElements.length) {
      return;
    }
    
    // Extract translation and status
    const translation = typeof response === 'object' ? response.translated : response;
    const status = typeof response === 'object' ? response.status : 'complete';
    const fromSource = typeof response === 'object' ? response.fromSource : false;
    
    if (!translation) {
      log(`No translation to update pending elements`);
      return;
    }
    
    log(`Updating ${pendingElements.length} pending elements with translation, status: ${status}`);
    
    pendingElements.forEach(info => {
      if (!info || !info.element) return;
      
      // Apply appropriate update based on element type
      switch (info.type) {
        case PENDING_TYPES.TEXT:
          // Update text content
          info.element.textContent = translation;
          log(`Updated text element with translation, key: ${info.key}`);
          
          // Only remove i18n tags if translation was successful
          if (status === 'complete' && !fromSource) {
            info.element.removeAttribute('data-i18n');
            
            // Also remove options if present
            if (info.element.hasAttribute('data-i18n-options')) {
              info.element.removeAttribute('data-i18n-options');
            }
            
            log(`Removed data-i18n attribute after pending update with confirmed translation`);
          } else {
            log(`Keeping data-i18n attribute after pending update, status: ${status}, fromSource: ${fromSource}`);
          }
          break;
          
        case PENDING_TYPES.ATTR:
          // Update attribute
          if (info.attr) {
            info.element.setAttribute(info.attr, translation);
            log(`Updated attribute ${info.attr} with translation, key: ${info.key}`);
            
            // Check if all attributes are translated before removing data-i18n-attr
            // Only remove if we have successful translation
            if (status === 'complete' && !fromSource) {
              const attrsJson = info.element.getAttribute('data-i18n-attr');
              if (attrsJson) {
                try {
                  const allAttrs = JSON.parse(attrsJson);
                  const attrKeys = Object.keys(allAttrs);
                  
                  // Only remove if this is the last attribute or if we're updating all at once
                  if (attrKeys.length === 1 || (pendingElements.length === attrKeys.length)) {
                    info.element.removeAttribute('data-i18n-attr');
                    
                    // Also remove options if present
                    if (info.element.hasAttribute('data-i18n-options')) {
                      info.element.removeAttribute('data-i18n-options');
                    }
                    
                    log(`Removed data-i18n-attr after updating all attributes`);
                  }
                } catch (e) {
                  logError('Error parsing data-i18n-attr during update:', e);
                }
              }
            } else {
              log(`Keeping data-i18n-attr, status: ${status}, fromSource: ${fromSource}`);
            }
          }
          break;
          
        case PENDING_TYPES.HTML:
          // Update HTML content
          info.element.innerHTML = translation;
          log(`Updated HTML element with translation, key: ${info.key}`);
          
          // Only remove i18n tags if translation was successful
          if (status === 'complete' && !fromSource) {
            info.element.removeAttribute('data-i18n-html');
            
            // Also remove options if present
            if (info.element.hasAttribute('data-i18n-options')) {
              info.element.removeAttribute('data-i18n-options');
            }
            
            log(`Removed data-i18n-html attribute after pending update with confirmed translation`);
          } else {
            log(`Keeping data-i18n-html attribute after pending update, status: ${status}, fromSource: ${fromSource}`);
          }
          break;
          
        default:
          logError(`Unknown pending element type: ${info.type}`);
      }
    });
  }
  
  /**
   * Обновляем все элементы с определенным ключом перевода
   * с удалением атрибута после подтвержденного перевода
   */
  function updateAllElementsWithKey(key, response) {
    log(`Обновляем все элементы с ключом "${key}" новым переводом`);
    
    // Извлекаем перевод и статус
    const translation = typeof response === 'object' ? response.translated : response;
    const status = typeof response === 'object' ? response.status : 'complete';
    const fromSource = typeof response === 'object' ? response.fromSource : false;
    
    if (!translation) {
      log(`Нет перевода для ключа "${key}" для обновления элементов`);
      return;
    }
    
    // Обрабатываем оба варианта ключей: с префиксом и без
    const keyWithoutPrefix = key.includes(':') ? key.split(':')[1] : key;
    
    let updateCount = 0;
    
    // Обновляем переводы текстового содержимого - с поддержкой индивидуальных OPTIONS
    document.querySelectorAll(`[data-i18n="${key}"], [data-i18n="${keyWithoutPrefix}"]`).forEach(element => {
      // Проверяем индивидуальные опции
      let options = {};
      try {
        const optionsAttr = element.getAttribute('data-i18n-options');
        if (optionsAttr) {
          options = JSON.parse(optionsAttr);
        }
      } catch (e) {
        logError('Ошибка при разборе data-i18n-options:', e);
      }
      
      // Применяем замену count, если необходимо
      if (options.count !== undefined && translation.includes('{{count}}')) {
        // Создаем индивидуальную копию перевода с замененным count
        const individualTranslation = translation.replace(/\{\{count\}\}/g, options.count);
        element.textContent = individualTranslation;
      } else {
        element.textContent = translation;
      }
      
      // Удаляем теги i18n только если перевод был успешным
      if (status === 'complete' && !fromSource) {
        element.removeAttribute('data-i18n');
        
        // Также удаляем опции, если они присутствуют
        if (element.hasAttribute('data-i18n-options')) {
          element.removeAttribute('data-i18n-options');
        }
        
        log(`Удален атрибут data-i18n для ключа "${key}" после пакетного обновления`);
      } else {
        log(`Сохраняем атрибут data-i18n для ключа "${key}", статус: ${status}, fromSource: ${fromSource}`);
      }
      
      updateCount++;
    });
    
    // Обновляем HTML переводы - то же самое с поддержкой опций
    document.querySelectorAll(`[data-i18n-html="${key}"], [data-i18n-html="${keyWithoutPrefix}"]`).forEach(element => {
      // Проверяем индивидуальные опции
      let options = {};
      try {
        const optionsAttr = element.getAttribute('data-i18n-options');
        if (optionsAttr) {
          options = JSON.parse(optionsAttr);
        }
      } catch (e) {
        logError('Ошибка при разборе data-i18n-options:', e);
      }
      
      if (options.count !== undefined && translation.includes('{{count}}')) {
        const individualTranslation = translation.replace(/\{\{count\}\}/g, options.count);
        element.innerHTML = individualTranslation;
      } else {
        element.innerHTML = translation;
      }
      
      // Удаляем теги i18n только если перевод был успешным
      if (status === 'complete' && !fromSource) {
        element.removeAttribute('data-i18n-html');
        
        // Также удаляем опции, если они присутствуют
        if (element.hasAttribute('data-i18n-options')) {
          element.removeAttribute('data-i18n-options');
        }
        
        log(`Удален атрибут data-i18n-html для ключа "${key}" после пакетного обновления`);
      } else {
        log(`Сохраняем атрибут data-i18n-html для ключа "${key}", статус: ${status}, fromSource: ${fromSource}`);
      }
      
      updateCount++;
    });
    
    // Обновляем переводы атрибутов
    document.querySelectorAll('[data-i18n-attr]').forEach(element => {
      try {
        const attrsJson = element.getAttribute('data-i18n-attr');
        if (!attrsJson) return;
        
        // Проверяем индивидуальные опции
        let options = {};
        try {
          const optionsAttr = element.getAttribute('data-i18n-options');
          if (optionsAttr) {
            options = JSON.parse(optionsAttr);
          }
        } catch (e) {
          logError('Ошибка при разборе data-i18n-options:', e);
        }
        
        const attrs = JSON.parse(attrsJson);
        let updatedAttrs = 0;
        const totalAttrs = Object.keys(attrs).length;
        
        for (const [attr, attrKey] of Object.entries(attrs)) {
          if (attrKey === key || attrKey === keyWithoutPrefix) {
            if (options.count !== undefined && translation.includes('{{count}}')) {
              const individualTranslation = translation.replace(/\{\{count\}\}/g, options.count);
              element.setAttribute(attr, individualTranslation);
            } else {
              element.setAttribute(attr, translation);
            }
            updatedAttrs++;
            updateCount++;
          }
        }
        
        // Удаляем data-i18n-attr только если все атрибуты были обновлены и перевод был успешным
        if (updatedAttrs === totalAttrs && status === 'complete' && !fromSource) {
          element.removeAttribute('data-i18n-attr');
          
          // Также удаляем опции, если они присутствуют
          if (element.hasAttribute('data-i18n-options')) {
            element.removeAttribute('data-i18n-options');
          }
          
          log(`Удален data-i18n-attr после обновления всех ${updatedAttrs} атрибутов для ключа "${key}"`);
        } else if (updatedAttrs > 0) {
          log(`Сохраняем data-i18n-attr для ключа "${key}", обновлено ${updatedAttrs}/${totalAttrs} атрибутов, статус: ${status}`);
        }
      } catch (error) {
        logError('Ошибка при обработке обновления перевода атрибута:', error);
      }
    });
    
    log(`Обновлено ${updateCount} элементов с ключом "${key}"`);
  }
  
  /**
   * Обновляем SVG языковые маски
   */
  function syncLanguageMasks() {
    try {
      const currentLang = getCurrentLanguage();
      log(`Синхронизация языковых масок для ${currentLang}`);
      
      // Закрываем все маски
      const supportedLangs = [
        'en', 'de', 'tr', 'pl', 'fr', 'it', 'es', 'ru', 'ar', 'zh', 'ro', 'hr', 'bg', 'hi', 'ja', 'ko', 'cs',
        'nl', 'el', 'pt', 'he', 'hu', 'sv', 'da', 'fi', 'sk', 'lt', 'lv', 'et', 'sl', 'uk', 'sr', 'bs', 'no'
      ];
      
      supportedLangs.forEach(lang => {
        const maskElement = document.getElementById(`${lang}Mask`);
        if (maskElement) {
          maskElement.setAttribute("mask", "url(#maskClose)");
        }
      });
      
      // Открываем маску текущего языка
      const currentMask = document.getElementById(`${currentLang}Mask`);
      if (currentMask) {
        currentMask.setAttribute("mask", "url(#maskOpen)");
      }
    } catch (e) {
      logError("Ошибка синхронизации языковых масок:", e);
    }
  }
  
  /**
   * Простая функция перевода - будет использовать кэш или текст по умолчанию
   */
  function t(key, options = {}) {
    const defaultValue = options.defaultValue || key;
    const language = getCurrentLanguage();
    
    // Пропускаем перевод для языка по умолчанию
    if (language === defaultLanguage) {
      return defaultValue;
    }
    
    // Применяем правило добавления namespace при необходимости
    const adjustedKey = maybeAddNamespace(key);
    
    // Извлекаем пространство имен
    let namespace = 'common';
    let translationKey = adjustedKey;
    
    if (adjustedKey.includes(':')) {
      const parts = adjustedKey.split(':');
      namespace = parts[0];
      translationKey = parts.slice(1).join(':');
    }
    
    // Проверяем кэш и возвращаем немедленно, если найдено
    const cacheKey = window.translationUtils ? 
        window.translationUtils.generateTranslationKey(defaultValue, language, namespace, options) :
        `${language}:${namespace}:${translationKey}`;
    const fullCacheKey = `${language}:${cacheKey}`;
    
    // Проверяем кэш и возвращаем немедленно, если найдено
    if (translationsCache[fullCacheKey]) {
      return translationsCache[fullCacheKey];
    }
    
    // Не в кэше, синхронно проверяем перевод на основе файла
    // Это делается синхронно, чтобы избежать Promise в функции t()
    let fileTranslation = null;
    
    // Простая синхронная проверка файла с использованием XMLHttpRequest
    if (!window.translationFileCache || 
        !window.translationFileCache.isFileMissing(`${language}:${namespace}`)) {
      try {
        const xhr = new XMLHttpRequest();
        xhr.open('GET', `/locales/${language}/${namespace}.json`, false); // Синхронный запрос
        xhr.send(null);
        
        if (xhr.status === 200) {
          const data = JSON.parse(xhr.responseText);
          // Переходим к вложенному ключу
          const keyParts = translationKey.split('.');
          let current = data;
          for (const part of keyParts) {
            if (!current || current[part] === undefined) {
              current = null;
              break;
            }
            current = current[part];
          }
          
          if (current && typeof current === 'string') {
            fileTranslation = current;
            
            // Обрабатываем опции
            if (options.count !== undefined && fileTranslation.includes('{{count}}')) {
              fileTranslation = fileTranslation.replace(/\{\{count\}\}/g, options.count);
            }
            
            // Кэшируем этот перевод
            translationsCache[fullCacheKey] = fileTranslation;
            return fileTranslation;
          }
        } else if (xhr.status === 404) {
          // Отмечаем файл как отсутствующий, чтобы предотвратить будущие запросы
          if (window.translationFileCache) {
            window.translationFileCache.markAsMissing(`${language}:${namespace}`);
          }
        }
      } catch (e) {
        // Тихий сбой - продолжаем с API запросом
      }
    }
    
    // Не в кэше, планируем асинхронную выборку для будущего использования
    if (!pendingTranslations[fullCacheKey]) {
      log(`Планируем асинхронную выборку для "${adjustedKey}"`);
      
      // Правильно инициализируем ожидающую запись
      pendingTranslations[fullCacheKey] = {
        request: null,
        elements: []
      };
      
      fetchTranslation(adjustedKey, defaultValue, options)
        .then(translation => {
          translationsCache[fullCacheKey] = translation.translated || translation;
          delete pendingTranslations[fullCacheKey];
          
          // Если перевод отличается, обновляем DOM элементы
          if ((translation.translated || translation) !== defaultValue) {
            updateAllElementsWithKey(adjustedKey, translation);
          }
        })
        .catch(() => {
          delete pendingTranslations[fullCacheKey];
        });
    }
    
    // Возвращаем значение по умолчанию пока что
    return defaultValue;
  }
  
  /**
   * Изменяем текущий язык
   */
  function setLanguage(langCode) {
    // Текущий язык
    const previousLanguage = getCurrentLanguage();
    
    // Если выбран тот же язык, ничего не делаем
    if (langCode === previousLanguage) {
      return;
    }
    
    log(`Изменяем язык с ${previousLanguage} на ${langCode}`);
    
    // Визуальные обновления для SVG масок
    try {
      document.getElementById(`${previousLanguage}Mask`).setAttribute("mask", "url(#maskClose)");
    } catch (e) { /* Тихий перехват */ }
    
    try {
      document.getElementById(`${langCode}Mask`).setAttribute("mask", "url(#maskOpen)");
    } catch (e) { /* Тихий перехват */ }
    
    // Сохраняем предпочтения языка в cookie и localStorage
    document.cookie = `i18next=${langCode}; path=/; max-age=${60 * 60 * 24 * 365}`;
    try {
      localStorage.setItem('i18nextLng', langCode);
    } catch (e) { /* Тихий перехват */ }
    
    // Добавляем параметр для предотвращения кэширования в URL и перезагружаем
    const cacheBuster = Date.now();
    const url = new URL(window.location.href);
    url.searchParams.set('i18n_cb', cacheBuster);
    url.searchParams.set('lang', langCode);
    
    window.location.href = url.toString();
  }
  
  // Экспортируем расширенный публичный API
  window.i18n = {
    init,
    t,
    getCurrentLanguage,
    changeLanguage: setLanguage,
    translateElement,
    translateDynamicElement,    // Для обратной совместимости
    updatePageTranslations: translatePageElements,
    syncLanguageMasks,
    isInitialized: () => isInitialized,
    
    /**
     * Загружает указанные пространства имен
     * @param {string|string[]} namespaces - Пространство имен или массив пространств имен
     * @returns {Promise<void>}
     */
    loadNamespaces: async function(namespaces) {
      const language = getCurrentLanguage();
      if (language === defaultLanguage) return Promise.resolve();
      
      const namespacesToLoad = Array.isArray(namespaces) ? namespaces : [namespaces];
      log(`Explicitly loading namespaces: ${namespacesToLoad.join(', ')} for ${language}`);
      
      await loadCommonNamespaces(language, namespacesToLoad);
      return Promise.resolve();
    },
    
    /**
     * Метод для установки режима совместимости
     * @param {boolean} enabled - Включить/выключить режим совместимости
     * @param {string} namespace - Пространство имен по умолчанию
     * @returns {void}
     */
    setCompatibilityMode: function(enabled, namespace = 'rma') {
      compatibilityMode.enabled = enabled;
      compatibilityMode.defaultNamespace = namespace;
      log(`Compatibility mode ${enabled ? 'enabled' : 'disabled'} with namespace '${namespace}'`);
    },
    
    // Для RMA-совместимости
    setRmaCompatibilityMode: (mode) => { rmaCompatibilityMode = mode },
    forceTriggerInitEvent: () => {
      const event = new CustomEvent('i18n:initialized', {
        detail: { language: getCurrentLanguage() }
      });
      document.dispatchEvent(event);
    }
  };
})();