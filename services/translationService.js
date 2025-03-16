// services/translationService.js
const { OpenAI } = require('openai');
const crypto = require('crypto');
const path = require('path');
const fs = require('fs');

// Инициализация OpenAI API
const openai = new OpenAI({
  apiKey: process.env.OPENAI_API_KEY
});

// Модель PostgreSQL для кэширования переводов
// Импортируем только если есть подключение к БД
let TranslationCache;
try {
  const { sequelize } = require('../models/postgresql');
  if (sequelize) {
    TranslationCache = require('../models/postgresql').TranslationCache;
  }
} catch (error) {
  console.warn('PostgreSQL not configured for translation cache. Using file-based cache only.');
}

// Директория для файлового кэша переводов
const CACHE_DIR = path.join(process.cwd(), 'cache', 'translations');

// Создаем директорию кэша, если её нет
if (!fs.existsSync(CACHE_DIR)) {
  fs.mkdirSync(CACHE_DIR, { recursive: true });
}

/**
 * Генерация уникального ключа для текста
 * @param {string} text - Исходный текст
 * @param {string} context - Контекст перевода
 * @returns {string} - MD5-хеш
 */
function generateKey(text, context = '') {
  return crypto
    .createHash('md5')
    .update(`${text}_${context}`)
    .digest('hex');
}

/**
 * Проверка кэша переводов
 * @param {string} text - Исходный текст
 * @param {string} targetLang - Целевой язык
 * @param {string} context - Контекст перевода
 * @returns {Promise<string|null>} - Перевод или null, если не найден
 */
async function checkCache(text, targetLang, context = '') {
  const key = generateKey(text, context);
  const cacheFile = path.join(CACHE_DIR, `${targetLang}_${key}.json`);
  
  // Сначала проверяем базу данных, если она доступна
  if (TranslationCache) {
    try {
      const cachedTranslation = await TranslationCache.findOne({
        where: {
          key,
          language: targetLang
        }
      });
      
      if (cachedTranslation) {
        // Обновляем счетчик использования и дату
        await cachedTranslation.update({
          lastUsed: new Date(),
          useCount: cachedTranslation.useCount + 1
        });
        
        return cachedTranslation.translatedText;
      }
    } catch (error) {
      console.error('Error checking database cache:', error);
    }
  }
  
  // Затем проверяем файловый кэш
  if (fs.existsSync(cacheFile)) {
    try {
      const cacheData = JSON.parse(fs.readFileSync(cacheFile, 'utf8'));
      return cacheData.translatedText;
    } catch (error) {
      console.error('Error reading file cache:', error);
    }
  }
  
  return null;
}

/**
 * Сохранение перевода в кэш
 * @param {string} text - Исходный текст
 * @param {string} targetLang - Целевой язык
 * @param {string} translatedText - Переведенный текст
 * @param {string} context - Контекст перевода
 */
async function saveToCache(text, targetLang, translatedText, context = '') {
  const key = generateKey(text, context);
  
  // Сохраняем в базу данных, если она доступна
  if (TranslationCache) {
    try {
      await TranslationCache.create({
        key,
        language: targetLang,
        originalText: text,
        translatedText: translatedText,
        context: context || null
      });
    } catch (error) {
      console.error('Error saving to database cache:', error);
    }
  }
  
  // Всегда сохраняем в файловый кэш как резервный вариант
  const cacheFile = path.join(CACHE_DIR, `${targetLang}_${key}.json`);
  const cacheData = {
    key,
    language: targetLang,
    originalText: text,
    translatedText: translatedText,
    context: context || null,
    timestamp: new Date().toISOString()
  };
  
  try {
    fs.writeFileSync(cacheFile, JSON.stringify(cacheData, null, 2), 'utf8');
  } catch (error) {
    console.error('Error saving to file cache:', error);
  }
}

/**
 * Перевод текста с помощью OpenAI
 * @param {string} text - Исходный текст
 * @param {string} targetLang - Целевой язык
 * @param {string} context - Контекст перевода
 * @param {string} sourceLang - Исходный язык (по умолчанию: de)
 * @returns {Promise<string>} - Переведенный текст
 */
async function translateWithAI(text, targetLang, context = '', sourceLang = 'de') {
  try {
    // Проверяем кэш перед API-вызовом
    const cachedTranslation = await checkCache(text, targetLang, context);
    if (cachedTranslation) return cachedTranslation;
    
    // Получаем полное название языка для более точного перевода
    const languageNames = {
      'de': 'German', 'en': 'English', 'fr': 'French', 'it': 'Italian',
      'es': 'Spanish', 'pt': 'Portuguese', 'nl': 'Dutch', 'da': 'Danish',
      'sv': 'Swedish', 'fi': 'Finnish', 'el': 'Greek', 'cs': 'Czech',
      'pl': 'Polish', 'hu': 'Hungarian', 'sk': 'Slovak', 'sl': 'Slovenian',
      'et': 'Estonian', 'lv': 'Latvian', 'lt': 'Lithuanian', 'ro': 'Romanian',
      'bg': 'Bulgarian', 'hr': 'Croatian', 'ga': 'Irish', 'mt': 'Maltese',
      'ru': 'Russian', 'tr': 'Turkish', 'ar': 'Arabic', 'zh': 'Chinese',
      'uk': 'Ukrainian', 'sr': 'Serbian', 'he': 'Hebrew', 'ko': 'Korean', 
      'ja': 'Japanese'
    };
    
    const targetLanguageName = languageNames[targetLang] || targetLang;
    const sourceLanguageName = languageNames[sourceLang] || sourceLang;
    
    // Создаем системный промпт с контекстом
    let systemPrompt = `You are a professional translator for a warehouse management system. 
Translate the text from ${sourceLanguageName} to ${targetLanguageName}. 
Maintain the same tone, formatting and technical terminology.
Ensure the translation sounds natural in the target language.`;
    
    if (context) {
      systemPrompt += `\nContext: This text appears in the "${context}" section of the application.`;
    }
    
    // Особые инструкции для специфических языков
    if (targetLang === 'ar' || targetLang === 'he') {
      systemPrompt += '\nNote: This language is read from right to left.';
    } else if (targetLang === 'zh') {
      systemPrompt += '\nUse Simplified Chinese characters.';
    } else if (targetLang === 'ja' || targetLang === 'ko') {
      systemPrompt += '\nPreserve technical terms in their standard form for this language.';
    }
    
    // Вызываем API OpenAI
    const response = await openai.chat.completions.create({
      model: "gpt-4o-mini", // или другая доступная модель
      messages: [
        { role: "system", content: systemPrompt },
        { role: "user", content: text }
      ],
      temperature: 0.3, // Низкая температура для более точных переводов
      max_tokens: Math.max(text.length * 2, 500) // Динамический лимит токенов
    });
    
    const translatedText = response.choices[0].message.content.trim();
    
    // Сохраняем в кэше
    await saveToCache(text, targetLang, translatedText, context);
    
    return translatedText;
  } catch (error) {
    console.error("Translation error:", error);
    // В случае ошибки возвращаем оригинальный текст
    return text;
  }
}

/**
 * Функция для пакетного перевода
 * @param {Array<string>} texts - Массив текстов для перевода
 * @param {string} targetLang - Целевой язык
 * @param {string} context - Контекст перевода
 * @param {string} sourceLang - Исходный язык (по умолчанию: de)
 * @returns {Promise<Array<string>>} - Массив переведенных текстов
 */
async function batchTranslate(texts, targetLang, context = '', sourceLang = 'de') {
  // Проверяем, есть ли в кэше все тексты
  const results = [];
  const missingTexts = [];
  const missingIndexes = [];
  
  for (let i = 0; i < texts.length; i++) {
    if (!texts[i] || texts[i].trim() === '') {
      results[i] = texts[i];
      continue;
    }
    
    const cachedText = await checkCache(texts[i], targetLang, context);
    if (cachedText) {
      results[i] = cachedText;
    } else {
      missingTexts.push(texts[i]);
      missingIndexes.push(i);
    }
  }
  
  // Если есть отсутствующие переводы, переводим их пакетом
  if (missingTexts.length > 0) {
    // Для оптимизации API-вызовов, разбиваем на пакеты по 20 текстов
    const BATCH_SIZE = 20;
    
    for (let batchStart = 0; batchStart < missingTexts.length; batchStart += BATCH_SIZE) {
      const batchEnd = Math.min(batchStart + BATCH_SIZE, missingTexts.length);
      const currentBatch = missingTexts.slice(batchStart, batchEnd);
      const currentIndexes = missingIndexes.slice(batchStart, batchEnd);
      
      const combinedText = currentBatch.join("\n---SEPARATOR---\n");
      
      // Получаем полные названия языков
      const languageNames = {
        'de': 'German', 'en': 'English', 'fr': 'French', 'it': 'Italian',
        'es': 'Spanish', 'pt': 'Portuguese', 'nl': 'Dutch', 'da': 'Danish',
        'sv': 'Swedish', 'fi': 'Finnish', 'el': 'Greek', 'cs': 'Czech',
        'pl': 'Polish', 'hu': 'Hungarian', 'sk': 'Slovak', 'sl': 'Slovenian',
        'et': 'Estonian', 'lv': 'Latvian', 'lt': 'Lithuanian', 'ro': 'Romanian',
        'bg': 'Bulgarian', 'hr': 'Croatian', 'ga': 'Irish', 'mt': 'Maltese',
        'ru': 'Russian', 'tr': 'Turkish', 'ar': 'Arabic', 'zh': 'Chinese',
        'uk': 'Ukrainian', 'sr': 'Serbian', 'he': 'Hebrew', 'ko': 'Korean', 
        'ja': 'Japanese'
      };
      
      const targetLanguageName = languageNames[targetLang] || targetLang;
      const sourceLanguageName = languageNames[sourceLang] || sourceLang;
      
      const systemPrompt = `You are a professional translator for a warehouse management system.
Translate the following texts from ${sourceLanguageName} to ${targetLanguageName}.
Each text is separated by "---SEPARATOR---".
Maintain the same tone, formatting and technical terminology.
Ensure the translations sound natural in ${targetLanguageName}.
Return ONLY the translated texts, with each separated by "---SEPARATOR---".
Keep the same order of texts.`;
      
      try {
        const response = await openai.chat.completions.create({
          model: "gpt-4o-mini", // или другая доступная модель
          messages: [
            { role: "system", content: systemPrompt },
            { role: "user", content: combinedText }
          ],
          temperature: 0.3
        });
        
        const translatedContent = response.choices[0].message.content.trim();
        const translatedTexts = translatedContent.split("---SEPARATOR---").map(t => t.trim());
        
        // Проверяем, что количество переведенных текстов совпадает с исходным
        if (translatedTexts.length === currentBatch.length) {
          // Сохраняем переводы в кэш и заполняем результаты
          for (let i = 0; i < currentBatch.length; i++) {
            const originalText = currentBatch[i];
            const translatedText = translatedTexts[i];
            
            // Сохранение в кэш
            await saveToCache(originalText, targetLang, translatedText, context);
            
            // Заполнение результатов
            results[currentIndexes[i]] = translatedText;
          }
        } else {
          // Если количество не совпадает, переводим по одному
          for (let i = 0; i < currentBatch.length; i++) {
            const translatedText = await translateWithAI(currentBatch[i], targetLang, context, sourceLang);
            results[currentIndexes[i]] = translatedText;
          }
        }
      } catch (error) {
        console.error("Batch translation error:", error);
        
        // В случае ошибки, переводим по одному
        for (let i = 0; i < currentBatch.length; i++) {
          const translatedText = await translateWithAI(currentBatch[i], targetLang, context, sourceLang);
          results[currentIndexes[i]] = translatedText;
        }
      }
    }
  }
  
  return results;
}

module.exports = {
  translateText: translateWithAI,
  batchTranslate,
  checkCache,
  saveToCache
};
