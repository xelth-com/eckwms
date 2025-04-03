// middleware/htmlTranslationInterceptor.js
const interceptor = require('express-interceptor');
const { stripBOM } = require('../utils/bomUtils');
const { checkCache, saveToCache } = require('../services/translationService');
const { generateTranslationKey } = require('../utils/translationKeys');
const path = require('path');
const fs = require('fs');

/**
 * Извлекает необходимые пространства имен из HTML-контента
 * @param {string} htmlContent - HTML-контент для анализа
 * @returns {string[]} - Массив пространств имен
 */
function extractRequiredNamespaces(htmlContent) {
  // Найти мета-тег с namespaces
  const nsMatch = htmlContent.match(/<meta\s+name=["']i18n-namespaces["']\s+content=["']([^"']+)["']\s*\/?>/i);
  if (nsMatch && nsMatch[1]) {
    return nsMatch[1].split(',').map(ns => ns.trim());
  }
  
  // Если мета-тег не найден, проверяем data-i18n атрибуты
  const namespaces = new Set(['common']); // common всегда загружаем
  
  // Ищем все атрибуты data-i18n и извлекаем namespace
  const i18nRegex = /data-i18n=["']([^"']+)["']/g;
  let match;
  while ((match = i18nRegex.exec(htmlContent)) !== null) {
    const key = match[1];
    if (key.includes(':')) {
      const namespace = key.split(':')[0];
      namespaces.add(namespace);
    }
  }
  
  // Также проверяем html и attr атрибуты
  const htmlRegex = /data-i18n-html=["']([^"']+)["']/g;
  while ((match = htmlRegex.exec(htmlContent)) !== null) {
    const key = match[1];
    if (key.includes(':')) {
      const namespace = key.split(':')[0];
      namespaces.add(namespace);
    }
  }
  
  const attrRegex = /data-i18n-attr=["'](.*?)["']/g;
  while ((match = attrRegex.exec(htmlContent)) !== null) {
    try {
      const attrObj = JSON.parse(match[1]);
      Object.values(attrObj).forEach(key => {
        if (key.includes(':')) {
          const namespace = key.split(':')[0];
          namespaces.add(namespace);
        }
      });
    } catch (e) {
      // Игнорируем ошибки парсинга JSON
    }
  }
  
  return Array.from(namespaces);
}

/**
 * Creates an HTML interceptor for i18n translation with improved placeholder handling
 * @param {Object} i18next - i18next instance
 * @returns {Function} Express middleware
 */
module.exports = function createHtmlTranslationInterceptor(i18next) {
  // Map of in-progress translations to prevent duplicate work
  const translationProcessingMap = new Map();
  // Clean up stale entries periodically
  setInterval(() => {
    const now = Date.now();
    const staleThreshold = 30000; // 30 seconds
    let cleaned = 0;

    translationProcessingMap.forEach((value, key) => {
      if (now - value.startTime > staleThreshold) {
        translationProcessingMap.delete(key);
        cleaned++;
      }
    });

    if (cleaned > 0) {
      console.log(`[i18n] Cleaned up ${cleaned} stale translation entries`);
    }
  }, 60000); // Run every minute

  return interceptor((req, res) => {
    return {
      // Only intercept HTML responses
      isInterceptable: () => {
        return /text\/html/.test(res.get('Content-Type')) ||
          !res.get('Content-Type'); // Also catch responses without content type
      },

      // This function is called after the response body is complete
      intercept: async (body, send) => {
        // Get language from request (set by i18next-http-middleware)
        const language = req.language || req.i18n?.language ||
          (process.env.DEFAULT_LANGUAGE || 'en');
        const defaultLanguage = process.env.DEFAULT_LANGUAGE || 'en';

        // Request-specific tracking to prevent cyclic translation attempts
        const processedKeys = new Set();

        console.log(`[i18n] Intercepting HTML response for language: ${language} (path: ${req.path})`);

        // Извлечь и предзагрузить необходимые пространства имен
        const namespaces = extractRequiredNamespaces(body);
        console.log(`[i18n] Identified namespaces for path ${req.path}: ${namespaces.join(', ')}`);
        
        // Add meta-tag with language for all languages
        let processedBody = body;
        if (body.includes('<head>') && !body.includes('meta name="app-language"')) {
          const languageMeta = `<meta name="app-language" content="${language}">`;
          processedBody = body.replace('<head>', `<head>\n    ${languageMeta}`);
        }

        // Skip translation for default language - just remove i18n attributes
        if (language === defaultLanguage) {
          console.log(`[i18n] Using default language (${defaultLanguage}), removing i18n tags`);

          // Remove data-i18n attributes, keeping content
          processedBody = processedBody.replace(
            /<([^>]+)\s+data-i18n="([^"]+)"([^>]*)>([^<]*)<\/([^>]+)>/g,
            (match, tag1, key, attrs, content, tag2) => {
              return `<${tag1}${attrs}>${content}</${tag2}>`;
            }
          );

          // Remove data-i18n-attr attributes, keeping translated attributes
          processedBody = processedBody.replace(
            /<([^>]+)\s+data-i18n-attr=['"]([^'"]+)['"]([^>]*)>/g,
            (match, tag, attrsJson, restAttrs) => {
              try {
                return `<${tag}${restAttrs}>`;
              } catch (e) {
                console.error('Error parsing data-i18n-attr:', e);
                return match;
              }
            }
          );

          // Remove data-i18n-html attributes, keeping HTML content
          processedBody = processedBody.replace(
            /<([^>]+)\s+data-i18n-html="([^"]+)"([^>]*)>([\s\S]*?)<\/([^>]+)>/g,
            (match, tag1, key, attrs, content, tag2) => {
              return `<${tag1}${attrs}>${content}</${tag2}>`;
            }
          );

          send(processedBody);
          return;
        }

        // For non-default languages - proceed with translations

        // Count translation tags to decide if we need to process
        const i18nTagCount = (processedBody.match(/data-i18n=/g) || []).length;
        const i18nAttrCount = (processedBody.match(/data-i18n-attr=/g) || []).length;
        const i18nHtmlCount = (processedBody.match(/data-i18n-html=/g) || []).length;

        console.log(`[i18n] Found translation tags: ${i18nTagCount} standard, ${i18nAttrCount} attribute, ${i18nHtmlCount} HTML`);

        if (i18nTagCount + i18nAttrCount + i18nHtmlCount === 0) {
          console.log('[i18n] No translation tags found, skipping translation');
          send(processedBody);
          return;
        }

        // Preload namespaces to ensure cache is fresh
        try {
          // Preload both detected namespaces and common ones for safety
          const combinedNamespaces = [...new Set([...namespaces, 'common', 'rma', 'dashboard', 'auth'])];
          
          // Для каждого namespace загрузить ресурсы
          await Promise.all(combinedNamespaces.map(async (ns) => {
            try {
              await i18next.reloadResources(language, ns);
              console.log(`[i18n] Preloaded namespace ${ns} for ${language}`);
              
              // Проверяем наличие файла перевода для namespace
              const filePath = path.join(process.cwd(), 'html', 'locales', language, `${ns}.json`);
              if (fs.existsSync(filePath)) {
                console.log(`[i18n] Translation file exists for ${language}:${ns}`);
              } else {
                console.warn(`[i18n] No translation file found for ${language}:${ns}`);
              }
            } catch (err) {
              console.warn(`[i18n] Error preloading namespace ${ns}: ${err.message}`);
            }
          }));
        } catch (error) {
          console.error('[i18n] Namespace preloading error:', error);
        }

        // Helper function to handle text translation with proper caching
        // Returns [translated_text, is_untranslated]
        async function translateText(originalText, key, namespace, options = {}) {
          if (!originalText || !originalText.trim()) {
            return [originalText, true]; // Untranslated
          }

          // Create unique key for tracking this translation
          const uniqueKey = `${language}:${namespace}:${key}`;

          // Skip if already processed in this request (prevents recursion)
          if (processedKeys.has(uniqueKey)) {
            return [originalText, true]; // Untranslated
          }
          processedKeys.add(uniqueKey);

          // Generate consistent cache key using the standardized function
          const cacheKey = generateTranslationKey(originalText, language, namespace, options);

          // Check if this translation is already in progress by another request
          if (translationProcessingMap.has(cacheKey)) {
            const inProgress = translationProcessingMap.get(cacheKey);

            // Wait for existing translation to complete if it's recent
            if (Date.now() - inProgress.startTime < 10000) { // 10 seconds
              try {
                console.log(`[i18n] Waiting for in-progress translation: ${cacheKey}`);
                await inProgress.promise;

                // Check cache again after waiting
                const cachedResult = await checkCache(originalText, language, namespace, options);
                if (cachedResult) {
                  console.log(`[i18n] Cache hit after waiting for: ${cacheKey}`);
                  return [cachedResult, false]; // Translated from cache
                }
              } catch (error) {
                console.warn(`[i18n] Error waiting for in-progress translation:`, error);
              }
            } else {
              // Stale entry, we'll proceed with translation
              console.log(`[i18n] Stale translation entry found for: ${cacheKey}`);
            }
          }

          // Check cache first
          try {
            const cachedTranslation = await checkCache(originalText, language, namespace, options);
            if (cachedTranslation) {
              console.log(`[i18n] Cache hit for: ${cacheKey}`);
              return [cachedTranslation, false]; // Translated from cache
            }
          } catch (cacheError) {
            console.error(`[i18n] Cache check error for ${cacheKey}:`, cacheError);
          }

          // Проверяем, существует ли ключ в других пространствах имен, если его нет в указанном
          // Перебираем все загруженные пространства имен, кроме текущего
          if (!key.includes(':') && namespaces.length > 1) {
            for (const ns of namespaces) {
              if (ns === namespace) continue; // Пропускаем текущий namespace, он уже проверен
              
              // Проверяем существование ключа в другом namespace
              const exists = i18next.exists(key, {
                ns: ns,
                lng: language
              });
              
              if (exists) {
                console.log(`[i18n] Found key ${key} in different namespace: ${ns}`);
                // Получаем перевод из найденного namespace
                const translation = i18next.t(key, {
                  ns: ns,
                  lng: language,
                  defaultValue: originalText,
                  ...options
                });
                
                // Сохраняем в кэш
                try {
                  await saveToCache(originalText, language, translation, ns, options);
                } catch (saveError) {
                  console.error(`[i18n] Error saving to cache:`, saveError);
                }
                
                return [translation, false]; // Помечаем как переведенный
              }
            }
          }

          // No cache hit - use i18next but first check if a key exists
          let translationPromise;
          try {
            // Create promise for potential concurrent requests
            translationPromise = new Promise(async (resolve, reject) => {
              try {
                // Check if translation exists in i18next resources
                const exists = i18next.exists(key, {
                  ns: namespace,
                  lng: language
                });

                // Get translation from i18next
                const translation = i18next.t(key, {
                  ns: namespace,
                  lng: language,
                  defaultValue: originalText, // Use original content as default
                  ...options // Pass through options like count
                });

                // If translation exists in resources, consider it translated
                // regardless of whether it matches the original text
                if (exists) {
                  try {
                    // Always save to cache if it exists in resources
                    await saveToCache(originalText, language, translation, namespace, options);
                    console.log(`[i18n] Saved to cache: ${cacheKey}`);
                  } catch (saveError) {
                    console.error(`[i18n] Error saving to cache for ${cacheKey}:`, saveError);
                  }

                  // Mark as translated because it exists in resources
                  resolve([translation, false]); // Translated
                } else {
                  // No translation found in resources, return original as untranslated
                  resolve([originalText, true]); // Untranslated
                }
              } catch (error) {
                console.error(`[i18n] Translation error for ${cacheKey}:`, error);
                reject(error);
              }
            });

            // Store in processing map for concurrent requests to reuse
            translationProcessingMap.set(cacheKey, {
              startTime: Date.now(),
              promise: translationPromise
            });

            // Wait for promise to resolve
            const result = await translationPromise;

            // Remove from processing map when done
            translationProcessingMap.delete(cacheKey);

            return result;
          } catch (error) {
            // Clean up processing map on error
            translationProcessingMap.delete(cacheKey);
            console.error(`[i18n] Translation error for ${uniqueKey}:`, error);
            return [originalText, true]; // Return original text as untranslated on error
          }
        }

        try {
          // Process regular translations with data-i18n attribute
          let modifiedBody = processedBody;

          // 1. Process regular translations with data-i18n attribute
          const textTranslationPromises = [];
          modifiedBody = modifiedBody.replace(
            /<([^>]+)\s+data-i18n="([^"]+)"([^>]*)>([^<]*)<\/([^>]+)>/g,
            (match, tag1, key, attrs, content, tag2) => {
              // Extract namespace and key
              let namespace = 'common';
              let translationKey = key;

              if (key.includes(':')) {
                const parts = key.split(':');
                namespace = parts[0];
                translationKey = parts.slice(1).join(':');
              }

              // Check for data-i18n-options attribute to handle count and other options
              let options = {};
              const optionsMatch = attrs.match(/data-i18n-options=['"]([^'"]+)['"]/);
              if (optionsMatch) {
                try {
                  options = JSON.parse(optionsMatch[1]);
                } catch (e) {
                  console.error(`[i18n] Error parsing data-i18n-options: ${e.message}`);
                }
              }

              // Store the match data for async processing
              const originalContent = content.trim();

              // Create a placeholder with unique ID for later replacement
              const placeholderId = `i18n_placeholder_${textTranslationPromises.length}`;

              // Add a promise to translate this text
              textTranslationPromises.push(
                translateText(originalContent, translationKey, namespace, options)
                  .then(([translatedText, untranslated]) => {
                    if (untranslated) {
                      // Keep the original tag with data-i18n attribute
                      return {
                        id: placeholderId,
                        html: match // Keep original with data-i18n attribute
                      };
                    } else {
                      // Remove data-i18n attribute since we have a translation
                      return {
                        id: placeholderId,
                        html: `<${tag1}${attrs.replace(/\s+data-i18n="[^"]*"/, '')}>${translatedText}</${tag2}>`
                      };
                    }
                  })
                  .catch(error => {
                    console.error(`[i18n] Error translating ${translationKey}:`, error);
                    return {
                      id: placeholderId,
                      html: match // Return original on error
                    };
                  })
              );

              // Return placeholder for now
              return `<!--${placeholderId}-->`;
            }
          );

          // 2. Process attribute translations with data-i18n-attr with better placeholder handling
          const attrTranslationPromises = [];
          modifiedBody = modifiedBody.replace(
            /<([^>]+)\s+data-i18n-attr=['"]([^'"]+)['"]([^>]*)>/g,
            (match, tag, attrsJson, restAttrs) => {
              try {
                // Create a placeholder with unique ID
                const placeholderId = `i18n_attr_placeholder_${attrTranslationPromises.length}`;

                // Parse attributes JSON
                const attrsMap = JSON.parse(attrsJson);

                // Get current attribute values - IMPORTANT FIX: Add default fallback
                const currentAttrs = {};
                const elementTags = ['input', 'textarea'];
                const defaultPlaceholders = {
                  'placeholder': {
                    'input': 'Enter value',
                    'textarea': 'Enter text here'
                  }
                };

                for (const attr of Object.keys(attrsMap)) {
                  // Extract current attribute value from element
                  const attrRegex = new RegExp(`${attr}="([^"]*)"`, 'i');
                  const attrMatch = match.match(attrRegex);
                  
                  // Check if we need to handle special default case for placeholder
                  let defaultValue = '';
                  
                  // IMPORTANT FIX: If attribute value is "null", try to use a reasonable default
                  if (!attrMatch || attrMatch[1] === 'null') {
                    // For placeholder attributes, use appropriate defaults based on element type
                    if (attr === 'placeholder') {
                      // Try to determine element type
                      const tagLower = tag.toLowerCase();
                      for (const elemTag of elementTags) {
                        if (tagLower.startsWith(elemTag)) {
                          defaultValue = defaultPlaceholders.placeholder[elemTag];
                          break;
                        }
                      }
                    }
                  } else {
                    defaultValue = attrMatch[1];
                  }
                  
                  currentAttrs[attr] = defaultValue;
                }

                // Create promise to translate all attributes
                attrTranslationPromises.push(
                  Promise.all(
                    Object.entries(attrsMap).map(async ([attr, key]) => {
                      // Extract namespace and key
                      let namespace = 'common';
                      let translationKey = key;

                      if (key.includes(':')) {
                        const parts = key.split(':');
                        namespace = parts[0];
                        translationKey = parts.slice(1).join(':');
                      }

                      // Use the default value or "null" string itself as fallback
                      const currentValue = currentAttrs[attr];
                      
                      // IMPORTANT FIX: Get default fallback value from i18next if possible
                      let defaultValue = currentValue;
                      if (currentValue === 'null' || !currentValue) {
                        if (i18next && typeof i18next.t === 'function') {
                          defaultValue = i18next.t(key, {
                            ns: namespace,
                            lng: language,
                            defaultValue: key.split('.').pop() // Use last part of key as default
                          });
                        }
                      }
                      
                      // Use appropriate value for translation
                      const valueToTranslate = (currentValue && currentValue !== 'null') 
                                               ? currentValue 
                                               : defaultValue;
                                               
                      const [translation, untranslated] = await translateText(valueToTranslate, translationKey, namespace);

                      return {
                        attr,
                        original: currentValue,
                        translated: translation,
                        untranslated
                      };
                    })
                  )
                    .then(translations => {
                      // Check if any attribute is untranslated
                      const anyUntranslated = translations.some(t => t.untranslated);
                      
                      if (anyUntranslated) {
                        // If any attribute was untranslated, keep the original tag
                        return {
                          id: placeholderId,
                          html: match
                        };
                      }

                      // All attributes translated, build new tag without data-i18n-attr
                      let newTag = `<${tag}${restAttrs.replace(/\s+data-i18n-attr=['"][^'"]+['"]/, '')}`;

                      // Apply translations to tag
                      translations.forEach(({ attr, original, translated }) => {
                        // Current attribute value
                        const attrRegex = new RegExp(`${attr}="([^"]*)"`, 'i');
                        const attrValueMatch = match.match(attrRegex);

                        // Replace or add attribute
                        if (attrValueMatch) {
                          newTag = newTag.replace(
                            `${attr}="${original}"`,
                            `${attr}="${translated}"`
                          );
                        } else {
                          newTag = newTag + ` ${attr}="${translated}"`;
                        }
                      });

                      return {
                        id: placeholderId,
                        html: newTag + '>'
                      };
                    })
                    .catch(error => {
                      console.error('[i18n] Error translating attributes:', error);
                      return {
                        id: placeholderId,
                        html: match // Return original on error
                      };
                    })
                );

                // Return placeholder for now
                return `<!--${placeholderId}-->`;
              } catch (error) {
                console.error('[i18n] Error parsing data-i18n-attr:', error);
                return match;
              }
            }
          );

          // 3. Process HTML content translations with data-i18n-html
          const htmlTranslationPromises = [];
          modifiedBody = modifiedBody.replace(
            /<([^>]+)\s+data-i18n-html="([^"]+)"([^>]*)>([\s\S]*?)<\/([^>]+)>/g,
            (match, tag1, key, attrs, content, tag2) => {
              // Create a placeholder with unique ID
              const placeholderId = `i18n_html_placeholder_${htmlTranslationPromises.length}`;

              // Extract namespace and key
              let namespace = 'common';
              let translationKey = key;

              if (key.includes(':')) {
                const parts = key.split(':');
                namespace = parts[0];
                translationKey = parts.slice(1).join(':');
              }

              // Check for options in attributes
              let options = {};
              const optionsMatch = attrs.match(/data-i18n-options=['"]([^'"]+)['"]/);
              if (optionsMatch) {
                try {
                  options = JSON.parse(optionsMatch[1]);
                } catch (e) {
                  console.error(`[i18n] Error parsing data-i18n-options for HTML ${key}:`, e.message);
                }
              }

              // Original content for comparison
              const originalContent = content.trim();

              // Add promise to translate HTML content
              htmlTranslationPromises.push(
                translateText(originalContent, translationKey, namespace, options)
                  .then(([translatedHtml, untranslated]) => {
                    if (untranslated) {
                      // Keep original with data-i18n-html for untranslated content
                      return {
                        id: placeholderId,
                        html: match
                      };
                    } else {
                      // Remove data-i18n-html for translated content
                      return {
                        id: placeholderId,
                        html: `<${tag1}${attrs.replace(/\s+data-i18n-html="[^"]*"/, '')}>${translatedHtml}</${tag2}>`
                      };
                    }
                  })
                  .catch(error => {
                    console.error(`[i18n] Error translating HTML for ${translationKey}:`, error);
                    return {
                      id: placeholderId,
                      html: match // Keep original on error
                    };
                  })
              );

              // Return placeholder for now
              return `<!--${placeholderId}-->`;
            }
          );

          // Wait for all translation promises to complete
          const allPromises = [
            ...textTranslationPromises,
            ...attrTranslationPromises,
            ...htmlTranslationPromises
          ];

          // Handle the case when there are no translation promises
          if (allPromises.length === 0) {
            console.log('[i18n] No actual translations to perform');
            send(modifiedBody);
            return;
          }

          console.log(`[i18n] Waiting for ${allPromises.length} translation operations to complete`);

          const startTime = Date.now();
          const results = await Promise.all(allPromises);
          const duration = Date.now() - startTime;

          console.log(`[i18n] Completed ${results.length} translations in ${duration}ms`);

          // Replace all placeholders with actual translations
          results.forEach(result => {
            modifiedBody = modifiedBody.replace(
              `<!--${result.id}-->`,
              result.html
            );
          });

          // Send the fully translated response
          send(modifiedBody);
        } catch (error) {
          console.error('[i18n] Fatal error in translation interceptor:', error);
          // On error, send the original body with meta tag but without translations
          send(processedBody);
        }
      }
    };
  });
};