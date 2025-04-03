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
  function getCurrentLanguage() {
    // Мета-тег имеет наивысший приоритет (предоставлен сервером)
    const metaLang = getLangFromMeta();
    if (metaLang) return metaLang;
    
    // Проверяем атрибут HTML lang
    const htmlLang = document.documentElement.lang;
    if (htmlLang) return htmlLang;
    
    // Проверяем cookie
    const cookieMatch = document.cookie.match(/i18next=([^;]+)/);
    if (cookieMatch) return cookieMatch[1];
    
    // Проверяем localStorage
    try {
      const lsLang = localStorage.getItem('i18nextLng');
      if (lsLang) return lsLang;
    } catch (e) {
      // Тихий перехват для ошибок localStorage
    }
    
    // Возвращаем язык по умолчанию
    return defaultLanguage;
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
      log('MutationObserver не поддерживается, пропускаем настройку динамического перевода');
      return;
    }
    
    log('Настраиваем MutationObserver для динамического контента');
    const observer = new MutationObserver(mutations => {
      let elementsToTranslate = 0;
      
      for (const mutation of mutations) {
        if (mutation.type === 'childList' && mutation.addedNodes.length) {
          for (const node of mutation.addedNodes) {
            if (node.nodeType === Node.ELEMENT_NODE) {
              if (needsTranslation(node)) {
                elementsToTranslate++;
                translateDynamicElement(node);
              }
            }
          }
        }
      }
      
      if (elementsToTranslate > 0) {
        log(`Переведено ${elementsToTranslate} динамически добавленных элементов`);
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
   * Переводим элемент и его дочерние элементы
   */
  function translateDynamicElement(element) {
    if (getCurrentLanguage() === defaultLanguage) return;
    
    // Обрабатываем стандартные переводы
    const standardElements = element.querySelectorAll('[data-i18n]');
    if (standardElements.length > 0) {
      log(`Найдено ${standardElements.length} элементов с data-i18n в динамическом контенте`);
      standardElements.forEach(processElementTranslation);
    }
    
    if (element.hasAttribute('data-i18n')) {
      processElementTranslation(element);
    }
    
    // Обрабатываем переводы атрибутов
    const attrElements = element.querySelectorAll('[data-i18n-attr]');
    if (attrElements.length > 0) {
      log(`Найдено ${attrElements.length} элементов с data-i18n-attr в динамическом контенте`);
      attrElements.forEach(processAttrTranslation);
    }
    
    if (element.hasAttribute('data-i18n-attr')) {
      processAttrTranslation(element);
    }
    
    // Обрабатываем HTML переводы
    const htmlElements = element.querySelectorAll('[data-i18n-html]');
    if (htmlElements.length > 0) {
      log(`Найдено ${htmlElements.length} элементов с data-i18n-html в динамическом контенте`);
      htmlElements.forEach(processHtmlTranslation);
    }
    
    if (element.hasAttribute('data-i18n-html')) {
      processHtmlTranslation(element);
    }
  }
  
  /**
   * Обрабатываем перевод текста элемента с удалением атрибута после подтвержденного перевода
   */
  function processElementTranslation(element) {
    const key = element.getAttribute('data-i18n');
    if (!key) return;
    
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
    
    log(`Переводим элемент с ключом: "${key}", текст: "${originalText.substring(0, 30)}${originalText.length > 30 ? '...' : ''}"`);
    
    // Информация об элементе для добавления в список ожидания
    const pendingInfo = {
      element: element,
      type: PENDING_TYPES.TEXT,
      key: key
    };
    
    // Сначала проверяем, существует ли перевод на основе файла - это приоритетнее API
    checkTranslationFile(key, options).then(fileTranslation => {
      if (fileTranslation) {
        log(`Применен перевод из файла для "${key}": "${fileTranslation}"`);
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
      fetchTranslation(key, originalText, options, pendingInfo).then(response => {
        // Ответ теперь должен включать как перевод, так и статус
        // Если нет ответа, сохраняем атрибут data-i18n
        if (!response) {
          log(`Нет ответа от API для "${key}", сохраняем атрибут data-i18n`);
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
          log(`Применен перевод API для "${key}": "${translation.substring(0, 30)}${translation.length > 30 ? '...' : ''}"`);
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
        logError(`Не удалось получить перевод для ключа "${key}":`, err);
        // При ошибке мы определенно хотим сохранить атрибут data-i18n
      });
    }).catch(err => {
      logError(`Ошибка при проверке перевода из файла для "${key}":`, err);
      
      // Возвращаемся к API переводу при ошибке
      fetchTranslation(key, originalText, options, pendingInfo).then(response => {
        // То же самое, что и выше
        if (!response) {
          log(`Нет ответа от API для "${key}" после ошибки файла, сохраняем атрибут data-i18n`);
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
   * Обрабатываем перевод атрибутов элемента с удалением атрибута после подтвержденного перевода
   */
  function processAttrTranslation(element) {
    try {
      const attrsJson = element.getAttribute('data-i18n-attr');
      if (!attrsJson) return;
      
      const attrs = JSON.parse(attrsJson);
      log(`Обрабатываем переводы атрибутов: ${attrsJson}`, element);
      
      // Отслеживаем успешные переводы
      const translationPromises = [];
      
      for (const [attr, key] of Object.entries(attrs)) {
        const originalValue = element.getAttribute(attr) || '';
        
        log(`Переводим атрибут "${attr}" с ключом "${key}", значение: "${originalValue}"`);
        
        // Информация об элементе для добавления в список ожидания
        const pendingInfo = {
          element: element,
          type: PENDING_TYPES.ATTR,
          attr: attr,
          key: key
        };
        
        // Создаем Promise для перевода этого атрибута
        const translationPromise = (async () => {
          // Сначала проверяем, существует ли перевод на основе файла
          try {
            const fileTranslation = await checkTranslationFile(key, {});
            if (fileTranslation) {
              log(`Применен перевод из файла для атрибута "${attr}": "${fileTranslation}"`);
              element.setAttribute(attr, fileTranslation);
              // Переводы на основе файлов считаются успешными
              return true;
            }
            
            // Нет перевода из файла, пробуем API
            const response = await fetchTranslation(key, originalValue, {}, pendingInfo);
            if (!response) {
              log(`Нет ответа от API для атрибута "${attr}", сохраняем атрибут`);
              return false;
            }
            
            // Извлекаем перевод и статус
            const translation = typeof response === 'object' ? response.translated : response;
            const status = typeof response === 'object' ? response.status : 'complete';
            const fromSource = typeof response === 'object' ? response.fromSource : false;
            
            if (translation) {
              element.setAttribute(attr, translation);
              log(`Применен перевод API для атрибута "${attr}": "${translation}"`);
            }
            
            // Успешно только если у нас есть перевод, статус complete, и не из исходного языка
            return (status === 'complete' && translation && !fromSource);
          } catch (err) {
            logError(`Ошибка при переводе атрибута "${attr}" с ключом "${key}":`, err);
            return false; // Не успешно переведено
          }
        })();
        
        translationPromises.push(translationPromise);
      }
      
      // После завершения всех переводов атрибутов, удаляем data-i18n-attr, если все были успешными
      Promise.all(translationPromises).then(results => {
        const allSuccessful = results.every(result => result === true);
        if (allSuccessful) {
          // Удаляем атрибут data-i18n-attr после успешного перевода всех атрибутов
          element.removeAttribute('data-i18n-attr');
          
          // Также удаляем опции, если они присутствуют
          if (element.hasAttribute('data-i18n-options')) {
            element.removeAttribute('data-i18n-options');
          }
          
          log(`Удален data-i18n-attr после успешного перевода всех атрибутов`);
        } else {
          log(`Сохраняем data-i18n-attr, потому что не все атрибуты были успешно переведены`);
        }
      });
    } catch (error) {
      logError('Ошибка при обработке перевода атрибута:', error);
    }
  }

  /**
   * Обрабатываем перевод HTML контента с удалением атрибута после подтвержденного перевода
   */
  function processHtmlTranslation(element) {
    const key = element.getAttribute('data-i18n-html');
    if (!key) return;
    
    const originalHtml = element.innerHTML.trim();
    
    log(`Переводим HTML контент с ключом: "${key}", длина: ${originalHtml.length} символов`);
    
    // Информация об элементе для добавления в список ожидания
    const pendingInfo = {
      element: element,
      type: PENDING_TYPES.HTML,
      key: key
    };
    
    // Сначала проверяем, существует ли перевод на основе файла
    checkTranslationFile(key, {}).then(fileTranslation => {
      if (fileTranslation) {
        log(`Применен HTML перевод из файла для "${key}", длина: ${fileTranslation.length} символов`);
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
      fetchTranslation(key, originalHtml, {}, pendingInfo).then(response => {
        // Обрабатываем ответ
        if (!response) {
          log(`Нет ответа от API для HTML "${key}", сохраняем атрибут`);
          return;
        }
        
        // Извлекаем перевод и статус
        const translation = typeof response === 'object' ? response.translated : response;
        const status = typeof response === 'object' ? response.status : 'complete';
        const fromSource = typeof response === 'object' ? response.fromSource : false;
        
        if (translation) {
          element.innerHTML = translation;
          log(`Применен HTML перевод API для "${key}", длина: ${translation.length} символов`);
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
        logError(`Не удалось получить HTML перевод для ключа "${key}":`, err);
        // При ошибке сохраняем атрибут data-i18n-html
      });
    }).catch(err => {
      logError(`Ошибка при проверке перевода из файла для HTML "${key}":`, err);
      
      // Возвращаемся к API переводу при ошибке
      fetchTranslation(key, originalHtml, {}, pendingInfo).then(response => {
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
  
  /**
   * Получаем перевод с улучшенной обработкой ответов
   * @param {string} key - Ключ перевода
   * @param {string} defaultText - Текст по умолчанию, если перевод не удался
   * @param {Object} options - Опции перевода (count и т.д.)
   * @param {Object} pendingInfo - Информация об элементе, который переводится
   * @returns {Promise<Object|string>} - Объект ответа со статусом или просто переведенный текст
   */
  async function fetchTranslation(key, defaultText, options = {}, pendingInfo = null) {
    const language = getCurrentLanguage();
    
    // Пропускаем перевод для языка по умолчанию
    if (language === defaultLanguage) {
      log(`Используем язык по умолчанию (${defaultLanguage}), пропускаем запрос перевода для "${key}"`);
      return {
        translated: defaultText,
        original: defaultText,
        language: language,
        fromSource: true, // Важный флаг, указывающий, что это из исходного языка
        status: 'complete'
      };
    }
    
    // Извлекаем пространство имен из ключа, если оно присутствует
    let namespace = 'common';
    let translationKey = key;
    
    if (key.includes(':')) {
      const parts = key.split(':');
      namespace = parts[0];
      translationKey = parts.slice(1).join(':');
    }
    
    // Используем тот же алгоритм, что и сервер, для генерации ключа кэша
    const cacheKey = window.translationUtils ? 
        window.translationUtils.generateTranslationKey(defaultText, language, namespace, options) :
        `${language}:${namespace}:${translationKey}`;
    
    // Добавляем префикс языка для клиентского кэша
    const fullCacheKey = `${language}:${cacheKey}`;
    
    log(`Запрос перевода для ключа: "${key}" с ключом кэша: "${fullCacheKey}"`);
    
    // Сначала проверяем клиентский кэш
    if (translationsCache[fullCacheKey]) {
      log(`Найдено в клиентском кэше: "${fullCacheKey}"`);
      return {
        translated: translationsCache[fullCacheKey],
        original: defaultText,
        language: language,
        fromCache: true,
        status: 'complete'
      };
    }
    
    // Добавляем запрос к уже ожидающему, если он существует
    if (pendingTranslations[fullCacheKey]) {
      log(`Перевод уже ожидает для "${fullCacheKey}", регистрируем элемент для последующего обновления`);
      
      // Если предоставлена информация об элементе, регистрируем его для последующего обновления
      if (pendingInfo) {
        // Создаем массив элементов, если он не существует
        if (!pendingTranslations[fullCacheKey].elements) {
          pendingTranslations[fullCacheKey].elements = [];
        }
        pendingTranslations[fullCacheKey].elements.push(pendingInfo);
      }
      
      // Возвращаем существующий Promise, чтобы избежать создания дубликатов запросов
      return pendingTranslations[fullCacheKey].request;
    }
    
    // Создаем новый Promise для запроса
    const translationPromise = new Promise(async (resolve, reject) => {
      try {
        log(`Делаем API запрос на перевод: "${key}" на языке: ${language}`);
        
        // Теперь пробуем API
        try {
          const response = await fetch(`/api/translate`, {
            method: 'POST',
            headers: {
              'Content-Type': 'application/json',
              'app-language': language
            },
            body: JSON.stringify({
              text: defaultText,
              targetLang: language,
              context: namespace,
              key: translationKey,
              defaultValue: defaultText,
              background: false,
              options: options
            })
          });
          
          const data = await response.json();
          
          log(`API ответ для "${key}": status=${response.status}, fromCache=${data.fromCache}, translated length=${data.translated?.length}`);
          
          // Обрабатываем статус "pending" с повторной попыткой
          if (response.status === 202 && data.status === 'pending') {
            const retryAfter = parseInt(response.headers.get('Retry-After') || '3', 10);
            
            log(`Перевод ожидает для "${key}", повторим через ${retryAfter} секунд`);
            
            // Планируем повторную попытку с предложенной сервером задержкой
            setTimeout(() => {
              log(`Повторяем перевод для "${key}" после задержки`);
              
              // Сохраняем ожидающие элементы для последующего обновления
              const pendingElements = pendingTranslations[fullCacheKey]?.elements || [];
              
              // Удаляем из ожидающих, чтобы разрешить новый запрос
              delete pendingTranslations[fullCacheKey];
              
              // Повторно запрашиваем перевод
              fetchTranslation(key, defaultText, options).then(translation => {
                log(`Повторная попытка успешна для "${key}", обновляем элементы`);
                
                // Обновляем все ожидающие элементы
                updateAllPendingElements(pendingElements, translation);
                
                // Разрешаем Promise с полученным переводом
                resolve(translation);
              }).catch(err => {
                logError(`Повторная попытка не удалась для "${key}":`, err);
                reject(err);
              });
            }, retryAfter * 1000);
            
            // Возвращаем объект ответа со статусом pending
            return {
              translated: defaultText, // Возвращаем оригинал как заполнитель
              original: defaultText,
              language: language,
              status: 'pending',
              retryAfter: retryAfter
            };
          }
          
          // Обрабатываем завершенный перевод
          if (data.translated) {
            const translation = data.translated;
            log(`Получен перевод для "${key}": "${translation.substring(0, 30)}${translation.length > 30 ? '...' : ''}"`);
            
            // Подготавливаем объект ответа
            const responseObj = {
              translated: translation,
              original: defaultText,
              language: language,
              fromCache: !!data.fromCache,
              fromSource: !!data.fromSource,
              status: data.status || 'complete'
            };
            
            // Кэшируем перевод, используя тот же формат ключа, что и бэкенд
            translationsCache[fullCacheKey] = translation;
            
            // Обновляем все ожидающие элементы
            if (pendingTranslations[fullCacheKey] && pendingTranslations[fullCacheKey].elements) {
              updateAllPendingElements(pendingTranslations[fullCacheKey].elements, responseObj);
            }
            
            // Очищаем статус ожидания
            delete pendingTranslations[fullCacheKey];
            
            // Также обновляем все элементы с этим ключом
            updateAllElementsWithKey(key, responseObj);
            
            // Разрешаем Promise с объектом ответа
            resolve(responseObj);
            return responseObj;
          }
          
          // Если мы дошли сюда, нет пригодного для использования перевода
          log(`Нет пригодного для использования перевода для "${key}", используем оригинальный текст`);
          delete pendingTranslations[fullCacheKey];
          
          // Возвращаем объект ответа со статусом ошибки
          const errorResponseObj = {
            translated: defaultText,
            original: defaultText,
            language: language,
            status: 'error',
            error: 'Данные перевода не возвращены'
          };
          
          resolve(errorResponseObj);
          return errorResponseObj;
        } catch (error) {
          logError(`API ошибка при переводе "${key}":`, error);
          delete pendingTranslations[fullCacheKey];
          
          // Возвращаем объект ответа с ошибкой
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
        logError(`Непредвиденная ошибка для "${key}":`, error);
        
        // Возвращаем объект ответа с ошибкой
        const errorResponseObj = {
          translated: defaultText,
          original: defaultText,
          language: language,
          status: 'error',
          error: 'Непредвиденная ошибка'
        };
        
        resolve(errorResponseObj);
        return errorResponseObj;
      }
    });
    
    // Сохраняем Promise и информацию об элементе в списке ожидания
    pendingTranslations[fullCacheKey] = {
      request: translationPromise,
      elements: pendingInfo ? [pendingInfo] : []
    };
    
    return translationPromise;
  }
  
  /**
   * Обновляем все ожидающие элементы, ожидающие перевода
   * с удалением атрибута после подтвержденного перевода
   */
  function updateAllPendingElements(pendingElements, response) {
    if (!pendingElements || !pendingElements.length) {
      return;
    }
    
    // Извлекаем перевод и статус
    const translation = typeof response === 'object' ? response.translated : response;
    const status = typeof response === 'object' ? response.status : 'complete';
    const fromSource = typeof response === 'object' ? response.fromSource : false;
    
    if (!translation) {
      log(`Нет перевода для обновления ожидающих элементов`);
      return;
    }
    
    log(`Обновляем ${pendingElements.length} ожидающих элементов переводом, статус: ${status}`);
    
    pendingElements.forEach(info => {
      if (!info || !info.element) return;
      
      // Применяем соответствующее обновление в зависимости от типа элемента
      switch (info.type) {
        case PENDING_TYPES.TEXT:
          // Обновляем текстовое содержимое
          info.element.textContent = translation;
          log(`Обновлен текстовый элемент переводом, ключ: ${info.key}`);
          
          // Удаляем теги i18n только если перевод был успешным
          if (status === 'complete' && !fromSource) {
            info.element.removeAttribute('data-i18n');
            
            // Также удаляем опции, если они присутствуют
            if (info.element.hasAttribute('data-i18n-options')) {
              info.element.removeAttribute('data-i18n-options');
            }
            
            log(`Удален атрибут data-i18n после ожидающего обновления с подтвержденным переводом`);
          } else {
            log(`Сохраняем атрибут data-i18n после ожидающего обновления, статус: ${status}, fromSource: ${fromSource}`);
          }
          break;
          
        case PENDING_TYPES.ATTR:
          // Обновляем атрибут
          if (info.attr) {
            info.element.setAttribute(info.attr, translation);
            log(`Обновлен атрибут ${info.attr} переводом, ключ: ${info.key}`);
            
            // Проверяем, все ли атрибуты переведены перед удалением data-i18n-attr
            // Удаляем только если у нас успешный перевод
            if (status === 'complete' && !fromSource) {
              const attrsJson = info.element.getAttribute('data-i18n-attr');
              if (attrsJson) {
                try {
                  const allAttrs = JSON.parse(attrsJson);
                  const attrKeys = Object.keys(allAttrs);
                  
                  // Удаляем только если это последний атрибут или если мы обновляем все сразу
                  if (attrKeys.length === 1 || (pendingElements.length === attrKeys.length)) {
                    info.element.removeAttribute('data-i18n-attr');
                    
                    // Также удаляем опции, если они присутствуют
                    if (info.element.hasAttribute('data-i18n-options')) {
                      info.element.removeAttribute('data-i18n-options');
                    }
                    
                    log(`Удален data-i18n-attr после обновления всех атрибутов`);
                  }
                } catch (e) {
                  logError('Ошибка при разборе data-i18n-attr во время обновления:', e);
                }
              }
            } else {
              log(`Сохраняем data-i18n-attr, статус: ${status}, fromSource: ${fromSource}`);
            }
          }
          break;
          
        case PENDING_TYPES.HTML:
          // Обновляем HTML содержимое
          info.element.innerHTML = translation;
          log(`Обновлен HTML элемент переводом, ключ: ${info.key}`);
          
          // Удаляем теги i18n только если перевод был успешным
          if (status === 'complete' && !fromSource) {
            info.element.removeAttribute('data-i18n-html');
            
            // Также удаляем опции, если они присутствуют
            if (info.element.hasAttribute('data-i18n-options')) {
              info.element.removeAttribute('data-i18n-options');
            }
            
            log(`Удален атрибут data-i18n-html после ожидающего обновления с подтвержденным переводом`);
          } else {
            log(`Сохраняем атрибут data-i18n-html после ожидающего обновления, статус: ${status}, fromSource: ${fromSource}`);
          }
          break;
          
        default:
          logError(`Неизвестный тип ожидающего элемента: ${info.type}`);
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
      const supportedLangs = ['de', 'en', 'fr', 'it', 'es', 'pt', 'nl', 'da', 'sv', 'fi', 
        'el', 'cs', 'pl', 'hu', 'sk', 'sl', 'et', 'lv', 'lt', 'ro',
        'bg', 'hr', 'ga', 'mt', 'ru', 'tr', 'ar', 'zh', 'uk', 'sr', 
        'he', 'ko', 'ja', 'no', 'bs', 'hi'];
      
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
    
    // Извлекаем пространство имен
    let namespace = 'common';
    let translationKey = key;
    
    if (key.includes(':')) {
      const parts = key.split(':');
      namespace = parts[0];
      translationKey = parts.slice(1).join(':');
    }
    
    // Поддерживаем оба стиля префиксации
    const fullKey = key.includes(':') ? key : `rma:${key}`;
    
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
      log(`Планируем асинхронную выборку для "${fullKey}"`);
      
      // Правильно инициализируем ожидающую запись
      pendingTranslations[fullCacheKey] = {
        request: null,
        elements: []
      };
      
      fetchTranslation(fullKey, defaultValue, options)
        .then(translation => {
          translationsCache[fullCacheKey] = translation.translated || translation;
          delete pendingTranslations[fullCacheKey];
          
          // Если перевод отличается, обновляем DOM элементы
          if ((translation.translated || translation) !== defaultValue) {
            updateAllElementsWithKey(fullKey, translation);
          }
        })
        .catch(() => {
          delete pendingTranslations[fullCacheKey];
        });
    }
    
    // Возвращаем значение по умолчанию пока что
    return defaultValue;
  }
  
  // Экспортируем публичный API
  window.i18n = {
    init,
    t,
    getCurrentLanguage,
    changeLanguage: setLanguage,
    translateDynamicElement,
    updatePageTranslations: translatePageElements,
    syncLanguageMasks,
    isInitialized: () => isInitialized,
    
    // Совместимость с RMA: экспортируем дополнительные функции
    setRmaCompatibilityMode: (mode) => { rmaCompatibilityMode = mode },
    forceTriggerInitEvent: () => {
      const event = new CustomEvent('i18n:initialized', {
        detail: { language: getCurrentLanguage() }
      });
      document.dispatchEvent(event);
    }
  };
  
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
})();