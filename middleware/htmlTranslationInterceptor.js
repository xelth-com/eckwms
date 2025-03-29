// middleware/htmlTranslationInterceptor.js
const interceptor = require('express-interceptor');
const { stripBOM, parseJSONWithBOM } = require('../utils/bomUtils');
const { checkCache, saveToCache } = require('../services/translationService');

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
        const language = req.language || req.i18n?.language || 
                        (process.env.DEFAULT_LANGUAGE || 'en');
        const defaultLanguage = process.env.DEFAULT_LANGUAGE || 'en';
        
        // Initialize tracking for processed keys to prevent infinite recursion
        req.processingKeys = req.processingKeys || new Set();
        
        // Add meta-tag with language for all languages
        let processedBody = body;
        if (body.includes('<head>') && !body.includes('meta name="app-language"')) {
          const languageMeta = `<meta name="app-language" content="${language}">`;
          processedBody = body.replace('<head>', `<head>\n    ${languageMeta}`);
        }
        
        // Special processing for default language
        if (language === defaultLanguage) {
          console.log(`[i18n] Using default language (${defaultLanguage}), removing i18n tags`);
          
          // Remove data-i18n attributes, keeping content
          processedBody = processedBody.replace(
            /<([^>]+)\s+data-i18n="([^"]+)"([^>]*)>([^<]*)<\/([^>]+)>/g,
            (match, tag1, key, attrs, content, tag2) => {
              // Just return element without data-i18n attribute
              return `<${tag1}${attrs}>${content}</${tag2}>`;
            }
          );
          
          // Remove data-i18n-attr attributes, keeping translated attributes
          processedBody = processedBody.replace(
            /<([^>]+)\s+data-i18n-attr=['"]([^'"]+)['"]([^>]*)>/g, 
            (match, tag, attrsJson, restAttrs) => {
              try {
                // Attributes should already be in English, so just keep them
                // and remove data-i18n-attr
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
              // Just return element without data-i18n-html attribute
              return `<${tag1}${attrs}>${content}</${tag2}>`;
            }
          );
          
          send(processedBody);
          return;
        }
        
        // For other languages - standard translation processing
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
        
        // NEW: Preload commonly used namespaces to ensure cache is fresh
        if (i18nTagCount + i18nAttrCount + i18nHtmlCount > 0) {
          try {
            // Preload common namespaces
            ['common', 'rma', 'dashboard', 'auth'].forEach(namespace => {
              i18next.reloadResources(language, namespace)
                .catch(err => console.error(`[i18n] Error preloading namespace ${namespace}:`, err));
            });
          } catch (error) {
            console.error('[i18n] Namespace preloading error:', error);
          }
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
                
                if (req.processingKeys.has(uniqueKey)) {
                  return match; // Skip if already processing this key
                }
                
                req.processingKeys.add(uniqueKey);
                
                // Remember original content for comparison
                const originalContent = content.trim();
                
                // Check for translation in cache first (new efficient approach)
                let translationPromise;
                const cacheKey = `${translationKey}_${language}_${namespace}`;
                
                // Using direct cache check for more efficiency
                checkCache(originalContent, language, namespace)
                  .then(cachedTranslation => {
                    if (cachedTranslation) {
                      console.log(`[i18n] Cache hit for ${uniqueKey}`);
                      return cachedTranslation;
                    }
                    
                    // Explicitly check if translation exists
                    const exists = i18next.exists(translationKey, { 
                      ns: namespace,
                      lng: language
                    });
                    
                    // Get translation
                    const translation = i18next.t(translationKey, { 
                      ns: namespace, 
                      lng: language,
                      defaultValue: originalContent // Use original content as default
                    });
                    
                    // Save to cache if it's a valid translation (not defaultValue)
                    if (exists && translation !== originalContent) {
                      saveToCache(originalContent, language, translation, namespace)
                        .catch(err => console.error('[i18n] Error saving to cache:', err));
                    }
                    
                    return translation;
                  })
                  .catch(err => {
                    console.error(`[i18n] Cache check error for ${uniqueKey}:`, err);
                    
                    // Fallback to i18next directly
                    return i18next.t(translationKey, {
                      ns: namespace,
                      lng: language,
                      defaultValue: originalContent
                    });
                  })
                  .finally(() => {
                    req.processingKeys.delete(uniqueKey);
                  });
                
                // Direct translation approach for server-side
                const exists = i18next.exists(translationKey, { 
                  ns: namespace,
                  lng: language
                });
                
                // Get translation
                const translation = i18next.t(translationKey, { 
                  ns: namespace, 
                  lng: language,
                  defaultValue: originalContent // Use original content as default
                });
                
                req.processingKeys.delete(uniqueKey);
                
                // CORRECT CHECK with parseMissingKeyHandler:
                // 1. Translation must exist in translation files
                // 2. And translation result must differ from original content
                if (exists && translation !== originalContent) {
                  // If translation exists and differs from original - remove tag
                  return `<${tag1}${attrs}>${translation}</${tag2}>`;
                } else {
                  // Otherwise return original tag
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
                let hasRealTranslations = false; // Flag for at least one real translation
                let allTranslated = true; // Flag for all attributes being translated
          
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
                  
                  // Directly check if translation exists
                  const exists = i18next.exists(translationKey, { 
                    ns: namespace,
                    lng: language
                  });
                  
                  // Get translation
                  const translation = i18next.t(translationKey, { 
                    ns: namespace, 
                    lng: language,
                    defaultValue: currentValue // Use current value as default
                  });
                  
                  // Check for real translation - exists and differs from current
                  const isRealTranslation = exists && translation !== currentValue;
                  
                  if (isRealTranslation) {
                    hasRealTranslations = true; // We have at least one translation
                  } else {
                    allTranslated = false; // Not all attributes translated
                  }
          
                  // Replace or add attribute with translation in any case
                  if (attrValueMatch) {
                    newTag = newTag.replace(
                      `${attr}="${currentValue}"`,
                      `${attr}="${translation}"`
                    );
                  } else {
                    newTag = newTag + ` ${attr}="${translation}"`;
                  }
                }
          
                // Remove data-i18n-attr tag only if:
                // 1. At least one real translation exists (to avoid removing if nothing translated)
                // 2. And all attributes have translations
                if (hasRealTranslations && allTranslated) {
                  return newTag + '>';
                } else {
                  // If something not translated or no translations at all,
                  // return updated tag with attributes but keep data-i18n-attr
                  return `${newTag} data-i18n-attr='${attrsJson}'>`;
                }
              } catch (e) {
                console.error('[i18n] Error parsing data-i18n-attr:', e);
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
                // Remember original content for comparison
                const originalContent = content.trim();
                
                // Check if translation exists directly
                const exists = i18next.exists(translationKey, { 
                  ns: namespace,
                  lng: language
                });
                
                // Get translation
                const translation = i18next.t(translationKey, { 
                  ns: namespace, 
                  lng: language,
                  defaultValue: originalContent, // Use original content as default
                  interpolation: { escapeValue: false } // Preserve HTML
                });
                
                // CORRECT CHECK with parseMissingKeyHandler:
                // 1. Translation must exist in translation files
                // 2. And translation result must differ from original content
                if (exists && translation !== originalContent) {
                  // If translation exists and differs from original - remove tag
                  return `<${tag1}${attrs}>${translation}</${tag2}>`;
                } else {
                  // Otherwise return original tag
                  return match;
                }
              } catch (error) {
                console.error(`[i18n] Error translating HTML ${translationKey}:`, error);
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