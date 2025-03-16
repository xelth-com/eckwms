// middleware/i18n.js
const i18next = require('i18next');
const i18nextMiddleware = require('i18next-http-middleware');
const Backend = require('i18next-fs-backend');
const path = require('path');

/**
 * Инициализация i18next для Express
 * @param {Object} options - Дополнительные настройки
 * @returns {Function} Middleware для Express
 */
function initI18n(options = {}) {
  const localesPath = path.join(process.cwd(), 'locales');
  
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
      fallbackLng: 'de',         // Немецкий как основной язык
      preload: supportedLngs,    // Предзагрузка всех языков
      ns: namespaces,            // Пространства имен
      defaultNS: 'common',       // Пространство имен по умолчанию
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
      load: 'languageOnly',      // Загружать только основной язык (de вместо de-DE)
      saveMissing: true,         // Сохранять отсутствующие переводы
      missingKeyHandler: (lng, ns, key, fallbackValue) => {
        if (process.env.NODE_ENV === 'development') {
          console.log(`Missing translation: [${lng}] ${ns}:${key}`);
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
  
  return i18nextMiddleware.handle(i18next);
}

module.exports = initI18n;
