// File: /packages/backend/src/utils/geminiErrorHandler.js

/**
 * Утилита для обработки и классификации ошибок Gemini API
 * Совместима с новым SDK @google/genai
 */

/**
 * Типы ошибок Gemini API (обновлено согласно официальной документации)
 */
const GEMINI_ERROR_TYPES = {
  RATE_LIMIT: 'RATE_LIMIT',              // Превышен лимит запросов (429)
  QUOTA_EXCEEDED: 'QUOTA_EXCEEDED',       // Превышена квота API
  INVALID_API_KEY: 'INVALID_API_KEY',     // Неверный API ключ (401)
  PERMISSION_DENIED: 'PERMISSION_DENIED', // Доступ запрещен (403)
  INVALID_ARGUMENT: 'INVALID_ARGUMENT',   // Неверные параметры запроса (400)
  NETWORK_ERROR: 'NETWORK_ERROR',         // Сетевая ошибка
  SERVICE_ERROR: 'SERVICE_ERROR',         // Ошибка сервиса Google (5xx)
  CONTENT_FILTER: 'CONTENT_FILTER',       // Контент заблокирован фильтром
  TIMEOUT_ERROR: 'TIMEOUT_ERROR',         // Превышен таймаут (504)
  CANCELLED: 'CANCELLED',                 // Запрос отменен (499)
  RECITATION: 'RECITATION',               // Остановка из-за сходства с данными
  UNKNOWN_ERROR: 'UNKNOWN_ERROR'          // Неизвестная ошибка
};

/**
 * Официальные статус коды Google Cloud API для Gemini
 */
const OFFICIAL_ERROR_CODES = {
  400: 'INVALID_ARGUMENT',
  401: 'UNAUTHENTICATED', 
  403: 'PERMISSION_DENIED',
  429: 'RESOURCE_EXHAUSTED',
  499: 'CANCELLED',
  500: 'INTERNAL',
  503: 'UNAVAILABLE', 
  504: 'DEADLINE_EXCEEDED'
};

/**
 * Паттерны для идентификации ошибок по сообщению (обновлено)
 */
const ERROR_PATTERNS = {
  [GEMINI_ERROR_TYPES.RATE_LIMIT]: [
    /rate limit exceeded/i,
    /too many requests/i,
    /resource exhausted/i,
    /quota.*exceeded.*requests/i,
    /RESOURCE_EXHAUSTED/i
  ],
  [GEMINI_ERROR_TYPES.QUOTA_EXCEEDED]: [
    /quota exceeded/i,
    /billing account/i,
    /usage limit/i,
    /free tier.*exceeded/i,
    /quota.*exceeded(?!.*requests)/i
  ],
  [GEMINI_ERROR_TYPES.INVALID_API_KEY]: [
    /invalid api key/i,
    /authentication failed/i,
    /UNAUTHENTICATED/i,
    /api key not valid/i,
    /unauthorized/i
  ],
  [GEMINI_ERROR_TYPES.PERMISSION_DENIED]: [
    /permission denied/i,
    /PERMISSION_DENIED/i,
    /access denied/i,
    /forbidden/i,
    /organization.*policy/i,
    /allowlisting/i
  ],
  [GEMINI_ERROR_TYPES.INVALID_ARGUMENT]: [
    /invalid argument/i,
    /INVALID_ARGUMENT/i,
    /malformed/i,
    /FAILED_PRECONDITION/i,
    /missing required field/i
  ],
  [GEMINI_ERROR_TYPES.NETWORK_ERROR]: [
    /network error/i,
    /connection.*failed/i,
    /timeout/i,
    /econnreset/i,
    /enotfound/i
  ],
  [GEMINI_ERROR_TYPES.SERVICE_ERROR]: [
    /internal server error/i,
    /service unavailable/i,
    /bad gateway/i,
    /temporarily unavailable/i,
    /INTERNAL/i,
    /UNAVAILABLE/i,
    /server error/i
  ],
  [GEMINI_ERROR_TYPES.CONTENT_FILTER]: [
    /content filter/i,
    /safety filter/i,
    /inappropriate content/i,
    /blocked.*policy/i,
    /safety.*setting/i,
    /BlockedReason/i
  ],
  [GEMINI_ERROR_TYPES.TIMEOUT_ERROR]: [
    /deadline exceeded/i,
    /DEADLINE_EXCEEDED/i,
    /request timeout/i
  ],
  [GEMINI_ERROR_TYPES.CANCELLED]: [
    /cancelled/i,
    /CANCELLED/i,
    /request.*cancelled/i
  ],
  [GEMINI_ERROR_TYPES.RECITATION]: [
    /recitation/i,
    /RECITATION/i,
    /resembles.*data/i
  ]
};

/**
 * Пользовательские сообщения для каждого типа ошибки (обновлено)
 */
const USER_MESSAGES = {
  [GEMINI_ERROR_TYPES.RATE_LIMIT]: {
    ru: "⚠️ Превышен лимит запросов к AI. Пожалуйста, подождите немного и попробуйте снова.",
    en: "⚠️ AI request limit exceeded. Please wait a moment and try again."
  },
  [GEMINI_ERROR_TYPES.QUOTA_EXCEEDED]: {
    ru: "⚠️ Исчерпана бесплатная квота AI на сегодня. Для продолжения работы рассмотрите возможность приобретения платного тарифа.",
    en: "⚠️ Daily AI quota exhausted. Consider upgrading to a paid plan to continue."
  },
  [GEMINI_ERROR_TYPES.INVALID_API_KEY]: {
    ru: "❌ Проблема с API ключом. Обратитесь к администратору системы.",
    en: "❌ API key issue. Contact system administrator."
  },
  [GEMINI_ERROR_TYPES.PERMISSION_DENIED]: {
    ru: "🚫 Доступ запрещен. Проверьте права доступа к модели или обратитесь к администратору.",
    en: "🚫 Access denied. Check model permissions or contact administrator."
  },
  [GEMINI_ERROR_TYPES.INVALID_ARGUMENT]: {
    ru: "⚠️ Некорректный запрос. Проверьте параметры и попробуйте снова.",
    en: "⚠️ Invalid request. Check parameters and try again."
  },
  [GEMINI_ERROR_TYPES.NETWORK_ERROR]: {
    ru: "🌐 Проблема с сетевым подключением. Проверьте интернет-соединение.",
    en: "🌐 Network connection issue. Check your internet connection."
  },
  [GEMINI_ERROR_TYPES.SERVICE_ERROR]: {
    ru: "🔧 Временная проблема с сервисом AI. Попробуйте позже.",
    en: "🔧 Temporary AI service issue. Try again later."
  },
  [GEMINI_ERROR_TYPES.CONTENT_FILTER]: {
    ru: "🛡️ Запрос заблокирован системой безопасности AI. Попробуйте перефразировать.",
    en: "🛡️ Request blocked by AI safety system. Try rephrasing."
  },
  [GEMINI_ERROR_TYPES.TIMEOUT_ERROR]: {
    ru: "⏱️ Превышено время ожидания. Попробуйте упростить запрос.",
    en: "⏱️ Request timeout. Try simplifying your request."
  },
  [GEMINI_ERROR_TYPES.CANCELLED]: {
    ru: "🚫 Запрос был отменен. Попробуйте снова.",
    en: "🚫 Request was cancelled. Try again."
  },
  [GEMINI_ERROR_TYPES.RECITATION]: {
    ru: "📝 Генерация остановлена из-за сходства с существующими данными. Измените запрос.",
    en: "📝 Generation stopped due to similarity with existing data. Modify your request."
  },
  [GEMINI_ERROR_TYPES.UNKNOWN_ERROR]: {
    ru: "❓ Неизвестная ошибка AI. Попробуйте снова или обратитесь к поддержке.",
    en: "❓ Unknown AI error. Try again or contact support."
  }
};

/**
 * Определяет тип ошибки по объекту ошибки (обновлено для @google/genai)
 * @param {Error} error - Объект ошибки
 * @returns {string} - Тип ошибки из GEMINI_ERROR_TYPES
 */
function classifyGeminiError(error) {
  if (!error) return GEMINI_ERROR_TYPES.UNKNOWN_ERROR;

  const errorMessage = error.message || '';
  const errorCode = error.code || error.status || error.statusCode || '';
  
  // Проверяем по HTTP статус коду (согласно официальной документации)
  const numericCode = parseInt(errorCode);
  
  if (numericCode === 429) {
    return GEMINI_ERROR_TYPES.RATE_LIMIT;
  }
  if (numericCode === 401) {
    return GEMINI_ERROR_TYPES.INVALID_API_KEY;
  }
  if (numericCode === 403) {
    return GEMINI_ERROR_TYPES.PERMISSION_DENIED;
  }
  if (numericCode === 400) {
    return GEMINI_ERROR_TYPES.INVALID_ARGUMENT;
  }
  if (numericCode === 499) {
    return GEMINI_ERROR_TYPES.CANCELLED;
  }
  if (numericCode === 504) {
    return GEMINI_ERROR_TYPES.TIMEOUT_ERROR;
  }
  if (numericCode >= 500 && numericCode < 600) {
    return GEMINI_ERROR_TYPES.SERVICE_ERROR;
  }

  // Проверяем по паттернам в сообщении об ошибке
  for (const [errorType, patterns] of Object.entries(ERROR_PATTERNS)) {
    for (const pattern of patterns) {
      if (pattern.test(errorMessage)) {
        return errorType;
      }
    }
  }

  // Проверяем специфичные свойства error объекта для @google/genai
  if (error.name === 'GoogleGenerativeAIError') {
    // Это специфичная ошибка от нового SDK
    if (errorMessage.includes('content filtering')) {
      return GEMINI_ERROR_TYPES.CONTENT_FILTER;
    }
  }

  return GEMINI_ERROR_TYPES.UNKNOWN_ERROR;
}

/**
 * Проверяет, является ли ошибка временной (требует повтора)
 * @param {string} errorType - Тип ошибки
 * @returns {boolean} - true если ошибка временная
 */
function isTemporaryError(errorType) {
  return [
    GEMINI_ERROR_TYPES.RATE_LIMIT,
    GEMINI_ERROR_TYPES.NETWORK_ERROR,
    GEMINI_ERROR_TYPES.SERVICE_ERROR,
    GEMINI_ERROR_TYPES.TIMEOUT_ERROR
  ].includes(errorType);
}

/**
 * Получает рекомендуемую задержку для повтора запроса
 * @param {string} errorType - Тип ошибки
 * @returns {number} - Задержка в секундах
 */
function getRetryDelay(errorType) {
  const delays = {
    [GEMINI_ERROR_TYPES.RATE_LIMIT]: 60,        // 1 минута для rate limit
    [GEMINI_ERROR_TYPES.NETWORK_ERROR]: 5,      // 5 секунд для сети
    [GEMINI_ERROR_TYPES.SERVICE_ERROR]: 30,     // 30 секунд для сервера
    [GEMINI_ERROR_TYPES.TIMEOUT_ERROR]: 10      // 10 секунд для таймаута
  };
  
  return delays[errorType] || 5;
}

/**
 * Получает пользовательское сообщение для типа ошибки
 * @param {string} errorType - Тип ошибки
 * @param {string} language - Язык ('ru' или 'en')
 * @returns {string} - Сообщение для пользователя
 */
function getUserMessage(errorType, language = 'ru') {
  const messages = USER_MESSAGES[errorType];
  if (!messages) {
    return USER_MESSAGES[GEMINI_ERROR_TYPES.UNKNOWN_ERROR][language];
  }
  return messages[language] || messages.ru;
}

/**
 * Основная функция обработки ошибок Gemini API
 * @param {Error} error - Объект ошибки
 * @param {Object} options - Опции обработки
 * @returns {Object} - Результат обработки ошибки
 */
function handleGeminiError(error, options = {}) {
  const { language = 'ru', includeRetryInfo = false } = options;
  
  const errorType = classifyGeminiError(error);
  const userMessage = getUserMessage(errorType, language);
  const isTemporary = isTemporaryError(errorType);
  const retryDelay = getRetryDelay(errorType);

  const result = {
    errorType,
    isTemporary,
    userMessage,
    originalError: error.message || 'Unknown error',
    httpStatus: error.code || error.status || error.statusCode
  };

  if (includeRetryInfo && isTemporary) {
    result.retryDelay = retryDelay;
    result.retryMessage = language === 'ru' 
      ? `Попробуйте снова через ${retryDelay} секунд.`
      : `Try again in ${retryDelay} seconds.`;
  }

  return result;
}

/**
 * Создает структурированный лог для ошибки Gemini API
 * @param {Error} error - Объект ошибки
 * @param {Object} context - Контекст (operation, userId и т.д.)
 * @returns {Object} - Объект для логирования
 */
function createGeminiErrorLog(error, context = {}) {
  const errorType = classifyGeminiError(error);
  const isTemporary = isTemporaryError(errorType);

  return {
    level: isTemporary ? 'warn' : 'error',
    msg: isTemporary 
      ? '🚦 Gemini API временная ошибка'
      : '❌ Gemini API критическая ошибка',
    geminiErrorType: errorType,
    isTemporary,
    userMessage: getUserMessage(errorType, 'ru'),
    originalError: error.message,
    httpStatus: error.code || error.status || error.statusCode,
    retryDelay: getRetryDelay(errorType),
    sdkVersion: '@google/genai', // Указываем используемый SDK
    ...context
  };
}

module.exports = {
  GEMINI_ERROR_TYPES,
  classifyGeminiError,
  isTemporaryError,
  getRetryDelay,
  getUserMessage,
  handleGeminiError,
  createGeminiErrorLog
};