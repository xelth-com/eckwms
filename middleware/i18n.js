// middleware/i18n.js
const i18next = require('i18next');
const i18nextMiddleware = require('i18next-http-middleware');
const Backend = require('i18next-fs-backend');
const path = require('path');
const { translateText, saveToCache } = require('../services/translationService');
const { Queue } = require('../utils/queue');

// Создаем очередь для отложенного перевода тегов
const translationQueue = new Queue();

// Запуск обработчика очереди переводов
function processTranslationQueue() {
  if (translationQueue.isEmpty()) {
    setTimeout(processTranslationQueue, 5000);
    return;
  }

  const { text, targetLang, namespace, key } = translationQueue.dequeue();
  
  // Выполняем перевод текста
  translateText(text, targetLang, namespace)
    .then(translatedText => {
      // Сохраняем перевод в файл локализации
      try {
        const filePath = path.join(process.cwd(), 'locales', targetLang, `${namespace}.json`);
        let translations = {};
        
        if (require('fs').existsSync(filePath)) {
          translations = require(filePath);
        }
        
        translations[key] = translatedText;
        require('fs').writeFileSync(filePath, JSON.stringify(translations, null, 2), 'utf8');
        
        // Обновляем кэш i18next
        i18next.addResourceBundle(targetLang, namespace, { [key]: translatedText }, true, true);
        
        console.log(`[i18n] Translated and saved: [${targetLang}] ${namespace}:${key}`);
      } catch (error) {
        console.error(`[i18n] Error saving translation: ${error.message}`);
      }
    })
    .catch(error => {
      console.error(`[i18n] Translation error: ${error.message}`);
    })
    .finally(() => {
      // Продолжаем обработку очереди с небольшой задержкой
      setTimeout(processTranslationQueue, 1000);
    });
}

/**
 * Инициализация i18next для Express
 * @param {Object} options - Дополнительные настройки
 * @returns {Function} Middleware для Express
 */
function initI18n(options = {}) {
  const localesPath = path.join(process.cwd(), 'locales');
  const defaultLanguage = process.env.DEFAULT_LANGUAGE || 'de';
  
  // Список всех поддерживаемых языков
  const supportedLngs = [
    // Официальные языки ЕС
    'de', 'en', 'fr', 'it', 'es', 'pt', 'nl', 'da', 'sv', 'fi', 
    'el', 'cs', 'pl', 'hu', 'sk', 'sl', 'et', 'lv', 'lt', 'ro', 
    'bg', 'hr', 'ga', 'mt',
    // Дополнительные языки
    'ru', 'tr', 'ar', 'zh', 'uk', 'sr', 'he', 'ko', 'ja'
  ];
  
  // Список пространств имен
  const namespaces = ['common', 'rma', 'dashboard', 'auth'];
  
  // Инициализация i18next
  i18next
    .use(Backend)
    .use(i18nextMiddleware.LanguageDetector)
    .init({
      backend: {
        loadPath: path.join(localesPath, '{{lng}}', '{{ns}}.json')
      },
      fallbackLng: defaultLanguage,     // Язык по умолчанию
      preload: supportedLngs,           // Предзагрузка всех языков
      ns: namespaces,                   // Пространства имен
      defaultNS: 'common',              // Пространство имен по умолчанию
      detection: {
        order: ['cookie', 'header', 'querystring', 'session'],
        lookupCookie: 'i18next',
        lookupQuerystring: 'lang',
        lookupHeader: 'accept-language',
        lookupSession: 'lang',
        caches: ['cookie'],
        cookieExpirationDate: new Date(Date.now() + 365 * 24 * 60 * 60 * 1000), // 1 год
        cookieDomain: options.cookieDomain || undefined
      },
      load: 'languageOnly',             // Загружать только основной язык (de вместо de-DE)
      saveMissing: true,                // Сохранять отсутствующие переводы
      parseMissingKeyHandler: (key, defaultValue) => {
        // Просто возвращаем ключ, чтобы сохранить теги для последующей обработки
        return key;
      },
      missingKeyHandler: (lng, ns, key, fallbackValue) => {
        // Если это не язык по умолчанию и ключ не был добавлен в очередь
        if (lng !== defaultLanguage) {
          // Добавляем в очередь для перевода
          translationQueue.enqueue({
            text: key,
            targetLang: lng,
            namespace: ns,
            key: key
          });
          
          if (process.env.NODE_ENV === 'development') {
            console.log(`[i18n] Added to translation queue: [${lng}] ${ns}:${key}`);
          }
        }
      },
      interpolation: {
        escapeValue: false,             // Не экранировать HTML
        formatSeparator: ',',
        format: function(value, format, lng) {
          // Специальная функция форматирования
          if (format === 'uppercase') return value.toUpperCase();
          return value;
        }
      },
      ...options
    });
  
  // Функция для получения перевода в промисной форме
  i18next.getTranslationAsync = async (key, options, lng) => {
    return new Promise((resolve) => {
      i18next.t(key, { ...options, lng }, (err, translation) => {
        resolve(translation);
      });
    });
  };
  
  // Запускаем обработчик очереди переводов
  processTranslationQueue();
  
  // Middleware для обработки HTML-ответов и замены тегов i18n
  const tagProcessor = (req, res, next) => {
    // Сохраняем оригинальную функцию send
    const originalSend = res.send;
    
    res.send = function(body) {
      // Обрабатываем только HTML-ответы
      if (typeof body === 'string' && 
          (res.get('Content-Type') || '').includes('text/html') || 
          body.includes('<!DOCTYPE html>') ||
          body.includes('<html>')) {
        
        // Получаем язык из запроса (установленный i18next-http-middleware)
        const language = req.language || defaultLanguage;
        
        if (language !== defaultLanguage) {
          // Заменяем теги data-i18n на переведенный текст
          body = body.replace(/<([^>]+)\s+data-i18n="([^"]+)"([^>]*)>([^<]*)<\/([^>]+)>/g, (match, tag1, key, attrs, content, tag2) => {
            // Пытаемся получить перевод
            const translation = req.i18n.t(key);
            
            // Если перевод совпадает с ключом (не найден), оставляем как есть
            if (translation === key) {
              return match;
            }
            
            // Заменяем содержимое тега на перевод
            return `<${tag1}${attrs}>${translation}</${tag2}>`;
          });
          
          // Обрабатываем атрибуты data-i18n-attr
          body = body.replace(/<([^>]+)\s+data-i18n-attr='([^']+)'([^>]*)>/g, (match, tag, attrsJson, restAttrs) => {
            try {
              const attrsMap = JSON.parse(attrsJson);
              let newTag = `<${tag}${restAttrs}`;
              
              for (const [attr, key] of Object.entries(attrsMap)) {
                const translation = req.i18n.t(key);
                
                // Получаем текущее значение атрибута, если есть
                const attrValueMatch = match.match(new RegExp(`${attr}="([^"]+)"`));
                const currentValue = attrValueMatch ? attrValueMatch[1] : '';
                
                // Если перевод отличается от ключа, заменяем значение атрибута
                if (translation !== key) {
                  newTag = newTag.replace(
                    `${attr}="${currentValue}"`, 
                    `${attr}="${translation}"`
                  );
                }
              }
              
              return newTag + '>';
            } catch (e) {
              console.error('Error parsing data-i18n-attr:', e);
              return match;
            }
          });
        }
      }
      
      // Вызываем оригинальную функцию send
      return originalSend.call(this, body);
    };
    
    next();
  };
  
  // Комбинируем middleware i18next и наш обработчик тегов
  return [i18nextMiddleware.handle(i18next), tagProcessor];
}

module.exports = initI18n;