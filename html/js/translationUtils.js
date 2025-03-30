// html/js/translationUtils.js
// Добавляем новый файл с функцией для генерации MD5 хеша, как в бэкенде

/**
 * Генерирует MD5 хеш для строки
 * @param {string} text - Строка для хеширования
 * @returns {string} - MD5 хеш
 */
function md5(text) {
  // Простая реализация MD5 для браузера
  // В реальном проекте лучше использовать проверенную библиотеку
  // В данном примере это упрощенная версия
  let hash = 0;
  if (text.length === 0) return hash.toString(16);
  
  for (let i = 0; i < text.length; i++) {
    const char = text.charCodeAt(i);
    hash = ((hash << 5) - hash) + char;
    hash = hash & hash; // Convert to 32bit integer
  }
  
  // Преобразуем хеш в 32-символьную строку для совместимости с MD5
  const hashStr = Math.abs(hash).toString(16);
  return '0'.repeat(32 - hashStr.length) + hashStr;
}

/**
 * Удаляет BOM из строки
 * @param {string} text - Исходный текст
 * @returns {string} - Текст без BOM
 */
function stripBOM(text) {
  if (text && typeof text === 'string' && text.charCodeAt(0) === 0xFEFF) {
    return text.slice(1);
  }
  return text;
}

/**
 * Генерирует ключ для кеширования перевода, используя тот же алгоритм, что и на сервере
 * @param {string} text - Исходный текст для перевода
 * @param {string} language - Целевой язык
 * @param {string} namespace - Пространство имен перевода
 * @param {Object} options - Дополнительные опции, влияющие на перевод
 * @returns {string} - Ключ кеша в формате MD5
 */
function generateTranslationKey(text, language, namespace = '', options = {}) {
  // Удаляем BOM перед генерацией ключа
  const cleanText = stripBOM(text);
  
  // Создаем базовую строку ключа
  let keyString = `${cleanText}_${namespace}_${language}`;
  
  // Добавляем опции к ключу, если они влияют на перевод
  if (options.count !== undefined) {
    keyString += `_count=${options.count}`;
  }
  
  // Добавьте другие опции по мере необходимости
  // if (options.gender) keyString += `_gender=${options.gender}`;
  // if (options.context) keyString += `_context=${options.context}`;
  
  // Генерируем MD5 хеш
  return md5(keyString);
}

// Экспортируем функции
window.translationUtils = {
  generateTranslationKey,
  stripBOM
};