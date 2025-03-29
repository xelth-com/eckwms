// middleware/htmlTranslationInterceptor.js
const interceptor = require('express-interceptor');
const { stripBOM, parseJSONWithBOM } = require('../utils/bomUtils');

/**
 * Creates an HTML interceptor for i18n translation
 * @param {Object} i18next - i18next instance
 * @returns {Function} Express middleware
 */
module.exports = function createHtmlTranslationInterceptor(i18next) {
  return interceptor((req, res) => {
    return {
      // Only intercept HTML responses
      isInterceptable: () => {
        return /text\/html/.test(res.get('Content-Type')) || 
               !res.get('Content-Type'); // Also catch responses without content type
      },
      
      // This function is called after the response body is complete
      intercept: (body, send) => {
        console.log('[i18n] Intercepted HTML response!');
        
        // Get language from request (set by i18next-http-middleware)
        const language = req.language || (process.env.DEFAULT_LANGUAGE || 'en');
        const defaultLanguage = process.env.DEFAULT_LANGUAGE || 'en';
        
        // Добавляем мета-тег с языком для всех языков
        let processedBody = body;
        if (body.includes('<head>') && !body.includes('meta name="app-language"')) {
          const languageMeta = `<meta name="app-language" content="${language}">`;
          processedBody = body.replace('<head>', `<head>\n    ${languageMeta}`);
        }
        
        // Особая обработка для английского (базового) языка
        if (language === defaultLanguage) {
          console.log(`[i18n] Using default language (${defaultLanguage}), removing i18n tags`);
          
          // Удаляем атрибуты data-i18n, сохраняя контент
          processedBody = processedBody.replace(
            /<([^>]+)\s+data-i18n="([^"]+)"([^>]*)>([^<]*)<\/([^>]+)>/g,
            (match, tag1, key, attrs, content, tag2) => {
              // Просто возвращаем элемент без атрибута data-i18n
              return `<${tag1}${attrs}>${content}</${tag2}>`;
            }
          );
          
          // Удаляем атрибуты data-i18n-attr, сохраняя переведенные атрибуты
          processedBody = processedBody.replace(
            /<([^>]+)\s+data-i18n-attr=['"]([^'"]+)['"]([^>]*)>/g, 
            (match, tag, attrsJson, restAttrs) => {
              try {
                // Атрибуты уже должны быть на английском, поэтому просто сохраняем их
                // и удаляем data-i18n-attr
                return `<${tag}${restAttrs}>`;
              } catch (e) {
                console.error('Error parsing data-i18n-attr:', e);
                return match;
              }
            }
          );
          
          // Удаляем атрибуты data-i18n-html, сохраняя HTML-контент
          processedBody = processedBody.replace(
            /<([^>]+)\s+data-i18n-html="([^"]+)"([^>]*)>([\s\S]*?)<\/([^>]+)>/g, 
            (match, tag1, key, attrs, content, tag2) => {
              // Просто возвращаем элемент без атрибута data-i18n-html
              return `<${tag1}${attrs}>${content}</${tag2}>`;
            }
          );
          
          send(processedBody);
          return;
        }
        
        // Для других языков - стандартная обработка перевода
        console.log(`[i18n] Processing HTML for language: ${language}`);
        
        // Count translation tags to see if any exist
        const i18nTagCount = (processedBody.match(/data-i18n=/g) || []).length;
        const i18nAttrCount = (processedBody.match(/data-i18n-attr=/g) || []).length;
        const i18nHtmlCount = (processedBody.match(/data-i18n-html=/g) || []).length;
        
        console.log(`[i18n] Found translation tags: ${i18nTagCount} standard, ${i18nAttrCount} attribute, ${i18nHtmlCount} HTML`);
        
        if (i18nTagCount + i18nAttrCount + i18nHtmlCount === 0) {
          console.log('[i18n] No translation tags found, skipping translation');
          send(processedBody);
          return;
        }
        
        try {
          // Process regular translations with data-i18n attribute
          let modifiedBody = processedBody.replace(
            /<([^>]+)\s+data-i18n="([^"]+)"([^>]*)>([^<]*)<\/([^>]+)>/g,
            (match, tag1, key, attrs, content, tag2) => {
              // Try to get translation - namespace may be included in the key
              let namespace = 'common';
              let translationKey = key;
              
              if (key.includes(':')) {
                const parts = key.split(':');
                namespace = parts[0];
                translationKey = parts.slice(1).join(':');
              }
              
              try {
                // Safeguard against infinite recursion with processing keys tracking
                const uniqueKey = `${language}:${namespace}:${translationKey}`;
                const processingKeys = req.processingKeys || new Set();
                
                if (processingKeys.has(uniqueKey)) {
                  return match; // Skip if already processing this key
                }
                
                processingKeys.add(uniqueKey);
                req.processingKeys = processingKeys;
                
                // Запоминаем оригинальный контент для сравнения
                const originalContent = content.trim();
                
                // Проверяем существование перевода напрямую
                const exists = i18next.exists(translationKey, { 
                  ns: namespace,
                  lng: language
                });
                
                // Получаем перевод
                const translation = i18next.t(translationKey, { 
                  ns: namespace, 
                  lng: language,
                  defaultValue: originalContent // Используем оригинальный контент как значение по умолчанию
                });
                
                processingKeys.delete(uniqueKey);
                
                // КОРРЕКТНАЯ ПРОВЕРКА с учетом parseMissingKeyHandler:
                // 1. Перевод должен существовать в файлах перевода
                // 2. И результат перевода должен отличаться от оригинального контента
                if (exists && translation !== originalContent) {
                  // Если есть перевод и он отличается от оригинала - удаляем тег
                  return `<${tag1}${attrs}>${translation}</${tag2}>`;
                } else {
                  // Иначе возвращаем оригинальный тег
                  return match;
                }
              } catch (error) {
                console.error(`[i18n] Error translating ${key}:`, error);
                return match; // Return original on error
              }
            }
          );
          
          // Process attribute translations with data-i18n-attr
          modifiedBody = modifiedBody.replace(
            /<([^>]+)\s+data-i18n-attr=['"]([^'"]+)['"]([^>]*)>/g, 
            (match, tag, attrsJson, restAttrs) => {
              try {
                const attrsMap = parseJSONWithBOM(attrsJson);
                let newTag = `<${tag}${restAttrs}`;
                let hasRealTranslations = false; // Флаг, есть ли хотя бы один настоящий перевод
                let allTranslated = true; // Флаг, все ли атрибуты переведены
          
                for (const [attr, key] of Object.entries(attrsMap)) {
                  // Namespace extraction
                  let namespace = 'common';
                  let translationKey = key;
          
                  if (key.includes(':')) {
                    const parts = key.split(':');
                    namespace = parts[0];
                    translationKey = parts.slice(1).join(':');
                  }
          
                  // Current attribute value
                  const attrRegex = new RegExp(`${attr}="([^"]*)"`, 'i');
                  const attrValueMatch = match.match(attrRegex);
                  const currentValue = attrValueMatch ? attrValueMatch[1] : '';
                  
                  // Проверяем существование перевода напрямую
                  const exists = i18next.exists(translationKey, { 
                    ns: namespace,
                    lng: language
                  });
                  
                  // Получаем перевод
                  const translation = req.i18n.t(translationKey, { 
                    ns: namespace, 
                    lng: language,
                    defaultValue: currentValue // Используем текущее значение как дефолт
                  });
                  
                  // Проверка реального перевода - перевод должен существовать и отличаться от текущего
                  const isRealTranslation = exists && translation !== currentValue;
                  
                  if (isRealTranslation) {
                    hasRealTranslations = true; // У нас есть хотя бы один перевод
                  } else {
                    allTranslated = false; // Не все атрибуты переведены
                  }
          
                  // Replace or add attribute с переводом в любом случае
                  if (attrValueMatch) {
                    newTag = newTag.replace(
                      `${attr}="${currentValue}"`,
                      `${attr}="${translation}"`
                    );
                  } else {
                    newTag = newTag + ` ${attr}="${translation}"`;
                  }
                }
          
                // Удаляем тег data-i18n-attr только если:
                // 1. Есть хотя бы один настоящий перевод (чтобы не удалять, если ничего не переведено)
                // 2. И все атрибуты имеют перевод
                if (hasRealTranslations && allTranslated) {
                  return newTag + '>';
                } else {
                  // Если что-то не переведено или нет ни одного перевода,
                  // возвращаем обновленный тег с атрибутами, но сохраняем data-i18n-attr
                  return `${newTag} data-i18n-attr='${attrsJson}'>`;
                }
              } catch (e) {
                console.error('Error parsing data-i18n-attr:', e);
                return match;
              }
            }
          );
          
          // Process HTML content translations with data-i18n-html
          modifiedBody = modifiedBody.replace(
            /<([^>]+)\s+data-i18n-html="([^"]+)"([^>]*)>([\s\S]*?)<\/([^>]+)>/g, 
            (match, tag1, key, attrs, content, tag2) => {
              // Namespace extraction
              let namespace = 'common';
              let translationKey = key;
          
              if (key.includes(':')) {
                const parts = key.split(':');
                namespace = parts[0];
                translationKey = parts.slice(1).join(':');
              }
          
              try {
                // Запоминаем оригинальный контент для сравнения
                const originalContent = content.trim();
                
                // Проверяем существование перевода напрямую
                const exists = i18next.exists(translationKey, { 
                  ns: namespace,
                  lng: language
                });
                
                // Получаем перевод
                const translation = req.i18n.t(translationKey, { 
                  ns: namespace, 
                  lng: language,
                  defaultValue: originalContent, // Используем оригинальный контент как дефолт
                  interpolation: { escapeValue: false } // Preserve HTML
                });
                
                // КОРРЕКТНАЯ ПРОВЕРКА с учетом parseMissingKeyHandler:
                // 1. Перевод должен существовать в файлах перевода
                // 2. И результат перевода должен отличаться от оригинального контента
                if (exists && translation !== originalContent) {
                  // Если есть перевод и он отличается от оригинала - удаляем тег
                  return `<${tag1}${attrs}>${translation}</${tag2}>`;
                } else {
                  // Иначе возвращаем оригинальный тег
                  return match;
                }
              } catch (error) {
                console.error(`Error translating HTML ${translationKey}:`, error);
                return match;
              }
            }
          );
          
          // Send the modified body
          send(modifiedBody);
        } catch (error) {
          console.error('[i18n] Error processing translations:', error);
          send(processedBody); // Send original body on error
        }
      }
    };
  });
};