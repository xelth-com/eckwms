// middleware/contentTypeDebugMiddleware
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
        
        // Skip processing if using default language
        if (language === defaultLanguage) {
          console.log(`[i18n] Using default language (${defaultLanguage}), skipping translation`);
          send(body);
          return;
        }
        
        // Process the complete HTML body for translations
        console.log(`[i18n] Processing HTML for language: ${language}`);
        
        // Count translation tags to see if any exist
        const i18nTagCount = (body.match(/data-i18n=/g) || []).length;
        const i18nAttrCount = (body.match(/data-i18n-attr=/g) || []).length;
        const i18nHtmlCount = (body.match(/data-i18n-html=/g) || []).length;
        
        console.log(`[i18n] Found translation tags: ${i18nTagCount} standard, ${i18nAttrCount} attribute, ${i18nHtmlCount} HTML`);
        
        if (i18nTagCount + i18nAttrCount + i18nHtmlCount === 0) {
          console.log('[i18n] No translation tags found, skipping translation');
          send(body);
          return;
        }
        
        try {
          
          
          // Process regular translations with data-i18n attribute
          let modifiedBody = body.replace(
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
                
                const translation = i18next.t(translationKey, { 
                  ns: namespace, 
                  lng: language,
                  defaultValue: content.trim() // This ensures the original text is used if no translation is found
                });
                
                processingKeys.delete(uniqueKey);
                
                // IMPORTANT: ALWAYS keep the data-i18n attribute for frontend to update later
                if (translation === translationKey) {
                  // Translation not found
                  return match;
                }
                
                // Return element with translation while keeping the data-i18n attribute
                return `<${tag1} data-i18n="${key}"${attrs}>${translation}</${tag2}>`;
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
                let allTranslated = true;
          
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
          
                  // Translation with default value
                  const translation = req.i18n.t(translationKey, { 
                    ns: namespace, 
                    lng: language,
                    defaultValue: currentValue // Original value as default
                  });
          
                  // Replace or add attribute
                  if (translation !== translationKey) {
                    if (attrValueMatch) {
                      newTag = newTag.replace(
                        `${attr}="${currentValue}"`,
                        `${attr}="${translation}"`
                      );
                    } else {
                      newTag = newTag + ` ${attr}="${translation}"`;
                    }
                  } else {
                    allTranslated = false;
                  }
                }
          
                return newTag + '>';
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
                // Translation with default value
                const translation = req.i18n.t(translationKey, { 
                  ns: namespace, 
                  lng: language,
                  defaultValue: content.trim(), // Original HTML content as default
                  interpolation: { escapeValue: false } // Preserve HTML
                });
          
                // If translation is the key, return original
                if (translation === translationKey) {
                  return match;
                }
          
                // Return translated HTML, keeping data-i18n-html attribute
                return `<${tag1} data-i18n-html="${key}"${attrs}>${translation}</${tag2}>`;
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
          send(body); // Send original body on error
        }
      }
    };
  });
};