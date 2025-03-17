// middleware/htmlInterceptor.js
const interceptor = require('express-interceptor');

/**
 * Create HTML interceptor middleware for translations
 * @param {Object} i18next - i18next instance
 * @returns {Function} Express middleware
 */
module.exports = function createHtmlInterceptor(i18next) {
  // Default language for fallback
  const defaultLanguage = process.env.DEFAULT_LANGUAGE || 'de';
  
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
        console.log('Body length:', body.length);
        
        // Process the complete HTML body for translations
        const language = req.language || defaultLanguage;
        
        if (language !== defaultLanguage) {
          console.log(`Processing translations for language: ${language}`);
          
          // Count translation tags to see if any exist
          const i18nTagCount = (body.match(/data-i18n=/g) || []).length;
          const i18nAttrCount = (body.match(/data-i18n-attr=/g) || []).length;
          const i18nHtmlCount = (body.match(/data-i18n-html=/g) || []).length;
          
          console.log(`Found translation tags: ${i18nTagCount} standard, ${i18nAttrCount} attribute, ${i18nHtmlCount} HTML`);
          
          if (i18nTagCount + i18nAttrCount + i18nHtmlCount > 0) {
            // Process simple elements with text content
            body = body.replace(/(<[^>]+data-i18n="([^"]+)"[^>]*>)([^<]*)<\/([^>]+)>/g, (match, openTag, key, content, closeTag) => {
              console.log(`Found translation tag with key: ${key}`);
              
              // Extract namespace and key
              let namespace = 'common';
              let translationKey = key;
              
              if (key.includes(':')) {
                const parts = key.split(':');
                namespace = parts[0];
                translationKey = parts.slice(1).join(':');
              }
              
              console.log(`Translating: ${translationKey} in namespace ${namespace}`);
              const translation = req.i18n.t(translationKey, { ns: namespace });
              
              // If translation equals key (not found), leave as is for frontend
              if (translation === translationKey) {
                console.log(`No translation found for ${key}`);
                return match;
              }
              
              console.log(`Translated: ${translationKey} → ${translation}`);
              // Remove data-i18n attribute
              return openTag.replace(/\s+data-i18n="[^"]+"/, '') + translation + '</' + closeTag + '>';
            });
            
            // Process data-i18n-attr attributes
            body = body.replace(/<([^>]+)\s+data-i18n-attr=['"]([^'"]+)['"]([^>]*)>/g, (match, tag, attrsJson, restAttrs) => {
              try {
                const attrsMap = JSON.parse(attrsJson);
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
                  
                  const translation = req.i18n.t(translationKey, { ns: namespace });
                  
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
                }
                
                // Keep data-i18n-attr for frontend retries if not all translated
                if (!allTranslated) {
                  return newTag + '>';
                }
                
                // Otherwise remove data-i18n-attr but keep the tag
                return newTag.replace(/\s+data-i18n-attr=['"][^'"]+['"]/, '') + '>';
              } catch (e) {
                console.error('Error parsing data-i18n-attr:', e);
                return match;
              }
            });
            
            // Process elements with HTML content
            body = body.replace(/<([^>]+)\s+data-i18n-html="([^"]+)"([^>]*)>([\s\S]*?)<\/([^>]+)>/g, (match, tag1, key, attrs, content, tag2) => {
              // Apply the same namespace extraction logic for HTML content
              let namespace = 'common';
              let translationKey = key;
              
              if (key.includes(':')) {
                const parts = key.split(':');
                namespace = parts[0];
                translationKey = parts.slice(1).join(':');
              }
              
              console.log(`Translating HTML: ${translationKey} in namespace ${namespace}`);
              const translation = req.i18n.t(translationKey, { 
                ns: namespace,
                interpolation: { escapeValue: false } 
              });
              
              // If translation equals key (not found), leave as is for frontend to handle
              if (translation === translationKey) {
                return match;
              }
              
              console.log(`Translated HTML: ${translationKey}`);
              // Return element with HTML translation and REMOVE the data-i18n-html attribute
              return `<${tag1}${attrs.replace(/\s+data-i18n-html="[^"]+"/, '')}>${translation}</${tag2}>`;
            });
            
            console.log('Translations applied!');
          } else {
            console.log('No translation tags found in HTML');
          }
        }
        
        // Send the modified body
        send(body);
      }
    };
  });
};
