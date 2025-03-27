// middleware/htmlInterceptor.js [UPDATED VERSION]
const interceptor = require('express-interceptor');
const { stripBOM, parseJSONWithBOM } = require('../utils/bomUtils');

/**
 * Create HTML interceptor middleware for translations and app configuration
 * @param {Object} i18next - i18next instance
 * @returns {Function} Express middleware
 */
module.exports = function createHtmlInterceptor(i18next) {
  // Default language for fallback
  const defaultLanguage = process.env.DEFAULT_LANGUAGE || 'en';

  return interceptor((req, res) => {
    // Only intercept HTML responses
    return {
      // Check if the response should be intercepted
      isInterceptable: () => {
        return /text\/html/.test(res.get('Content-Type')) ||
          !res.get('Content-Type'); // Also catch responses without content type
      },

      // This function is called after the response body is complete
      intercept: (body, send) => {
        console.log('Intercepted HTML response!');

        // Initialize request-specific data storage instead of globals
        req.elementContents = new Map();
        req.currentProcessingHtml = body;

        // Modify HTML to inject app configuration
        let modifiedBody = body;
        if (modifiedBody.includes('<head>')) {
          // Build application configuration object
          const appConfig = {
            DEFAULT_LANGUAGE: process.env.DEFAULT_LANGUAGE || 'en',
            NODE_ENV: process.env.NODE_ENV || 'development',
            // Add any other configuration you need
            API_BASE_URL: process.env.API_BASE_URL || '',
            APP_VERSION: process.env.npm_package_version || '1.0.0'
          };
          
          const configScript = `<head>
<script>
// Global app configuration
window.APP_CONFIG = ${JSON.stringify(appConfig)};
console.log("App config loaded:", window.APP_CONFIG);
</script>`;
          
          modifiedBody = modifiedBody.replace('<head>', configScript);
        }

        // Process the complete HTML body for translations
        const language = req.language || defaultLanguage;

        // Only perform translations when NOT using default language
        if (language !== defaultLanguage) {
          console.log(`Processing translations for language: ${language}`);

          // Count translation tags to see if any exist
          const i18nTagCount = (modifiedBody.match(/data-i18n=/g) || []).length;
          const i18nAttrCount = (modifiedBody.match(/data-i18n-attr=/g) || []).length;
          const i18nHtmlCount = (modifiedBody.match(/data-i18n-html=/g) || []).length;

          console.log(`Found translation tags: ${i18nTagCount} standard, ${i18nAttrCount} attribute, ${i18nHtmlCount} HTML`);

          if (i18nTagCount + i18nAttrCount + i18nHtmlCount > 0) {
            // Make current HTML available for the missingKeyHandler via request
            req.currentProcessingHtml = modifiedBody;
            
            // First step: Extract all content from elements with data-i18n attributes
            // This pre-processing step makes content available for missingKeyHandler
            modifiedBody.replace(/<([^>]+)\s+data-i18n="([^"]+)"([^>]*)>([^<]*)<\/([^>]+)>/g, 
              (match, tag1, key, attrs, content, tag2) => {
                if (content && content.trim()) {
                  // Store the content by key for the missingKeyHandler to use
                  req.elementContents.set(key, content.trim());
                  if (process.env.NODE_ENV === 'development') {
                    console.log(`Stored content for key ${key}: "${content.trim()}"`);
                  }
                }
                return match; // Return unchanged, this is just for extraction
              }
            );
            
            // Process regular translations (data-i18n attribute)
            modifiedBody = modifiedBody.replace(/<([^>]+)\s+data-i18n="([^"]+)"([^>]*)>([^<]*)<\/([^>]+)>/g, (match, tag1, key, attrs, content, tag2) => {
              // Try to get translation - namespace may be included in the key
              let namespace = 'common';
              let translationKey = key;

              if (key.includes(':')) {
                const parts = key.split(':');
                namespace = parts[0];
                translationKey = parts.slice(1).join(':');
              }

              console.log(`Translating: ${translationKey} in namespace ${namespace}`);
              
              try {
                // Add safeguard against infinite recursion
                const uniqueKey = `${language}:${namespace}:${translationKey}`;
                const processingKeys = req.processingKeys || new Set();
                
                if (processingKeys.has(uniqueKey)) {
                  return match; // Skip if already processing this key
                }
                
                processingKeys.add(uniqueKey);
                req.processingKeys = processingKeys;
                
                const translation = i18next.t(translationKey, { ns: namespace });
                
                processingKeys.delete(uniqueKey);
                req.processingKeys = processingKeys;

                // IMPORTANT: We ALWAYS keep the data-i18n attribute
                // so frontend can update it later when translation becomes available
                if (translation === translationKey) {
                  // Translation not found - missingKeyHandler will queue it
                  return match; // Return original content with data-i18n tag
                }

                console.log(`Translated: ${translationKey} → ${translation}`);
                
                // Return element with translation while KEEPING the data-i18n attribute
                return `<${tag1} data-i18n="${key}"${attrs}>${translation}</${tag2}>`;
              } catch (error) {
                console.error(`Error translating ${translationKey}:`, error);
                return match; // Return original on error
              }
            });

            // Process attribute translations (data-i18n-attr)
            modifiedBody = modifiedBody.replace(/<([^>]+)\s+data-i18n-attr=['"]([^'"]+)['"]([^>]*)>/g, (match, tag, attrsJson, restAttrs) => {
              try {
                // Use BOM-aware JSON parsing
                const attrsMap = parseJSONWithBOM(attrsJson);
                let newTag = `<${tag}${restAttrs}`;
                let allTranslated = true;

                for (const [attr, key] of Object.entries(attrsMap)) {
                  console.log(`Translating attribute: ${attr} with key ${key}`);
                  // Extract namespace if present
                  let namespace = 'common';
                  let translationKey = key;

                  if (key.includes(':')) {
                    const parts = key.split(':');
                    namespace = parts[0];
                    translationKey = parts.slice(1).join(':');
                  }

                  // Add safeguard against infinite recursion
                  const uniqueKey = `${language}:${namespace}:${translationKey}`;
                  const processingKeys = req.processingKeys || new Set();
                  
                  if (processingKeys.has(uniqueKey)) {
                    allTranslated = false;
                    continue; // Skip if already processing this key
                  }
                  
                  try {
                    processingKeys.add(uniqueKey);
                    req.processingKeys = processingKeys;
                    
                    const translation = i18next.t(translationKey, { ns: namespace });
                    
                    processingKeys.delete(uniqueKey);
                    req.processingKeys = processingKeys;

                    // Get current attribute value if present
                    const attrRegex = new RegExp(`${attr}="([^"]*)"`, 'i');
                    const attrValueMatch = match.match(attrRegex);
                    const currentValue = attrValueMatch ? attrValueMatch[1] : '';

                    // If translation differs from key, replace attribute value
                    if (translation !== translationKey) {
                      console.log(`Translated attr: ${translationKey} → ${translation}`);
                      if (attrValueMatch) {
                        newTag = newTag.replace(
                          `${attr}="${currentValue}"`,
                          `${attr}="${translation}"`
                        );
                      } else {
                        // Attribute not present, add it
                        newTag = newTag + ` ${attr}="${translation}"`;
                      }
                    } else {
                      allTranslated = false; // Mark that not all attributes are translated
                    }
                  } catch (error) {
                    console.error(`Error translating attribute ${translationKey}:`, error);
                    allTranslated = false;
                  }
                }

                // IMPORTANT: Always keep data-i18n-attr for frontend retries
                return newTag + '>';
              } catch (e) {
                console.error('Error parsing data-i18n-attr:', e);
                return match;
              }
            });

            // Process HTML content translations (data-i18n-html)
            modifiedBody = modifiedBody.replace(/<([^>]+)\s+data-i18n-html="([^"]+)"([^>]*)>([\s\S]*?)<\/([^>]+)>/g, (match, tag1, key, attrs, content, tag2) => {
              // Apply the same namespace extraction logic for HTML content
              let namespace = 'common';
              let translationKey = key;

              if (key.includes(':')) {
                const parts = key.split(':');
                namespace = parts[0];
                translationKey = parts.slice(1).join(':');
              }

              console.log(`Translating HTML: ${translationKey} in namespace ${namespace}`);
              
              try {
                // Add safeguard against infinite recursion
                const uniqueKey = `${language}:${namespace}:${translationKey}`;
                const processingKeys = req.processingKeys || new Set();
                
                if (processingKeys.has(uniqueKey)) {
                  return match; // Skip if already processing this key
                }
                
                processingKeys.add(uniqueKey);
                req.processingKeys = processingKeys;
                
                const translation = i18next.t(translationKey, {
                  ns: namespace,
                  interpolation: { escapeValue: false }
                });
                
                processingKeys.delete(uniqueKey);
                req.processingKeys = processingKeys;

                // Store the HTML content for potential translation
                if (content && content.trim()) {
                  req.elementContents.set(key, content.trim());
                }

                // ALWAYS keep the data-i18n-html attribute for frontend to update later
                if (translation === translationKey) {
                  return match; // Keep original with attribute for frontend to find
                }

                console.log(`Translated HTML: ${translationKey}`);
                // Return element with HTML translation while KEEPING the data-i18n-html attribute
                return `<${tag1} data-i18n-html="${key}"${attrs}>${translation}</${tag2}>`;
              } catch (error) {
                console.error(`Error translating HTML ${translationKey}:`, error);
                return match; // Return original on error
              }
            });

            // No need to clean up request-specific variables
          } else {
            console.log('No translation tags found in HTML');
          }
        } else {
          console.log(`Using default language (${defaultLanguage}), no translation needed`);
        }

        // Send the modified body
        send(modifiedBody);
      }
    };
  });
};