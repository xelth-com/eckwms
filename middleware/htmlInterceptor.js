// middleware/htmlInterceptor.js
const interceptor = require('express-interceptor');

/**
 * Create HTML interceptor middleware for translations
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
        console.log('Body length:', body.length);

        // Process the complete HTML body for translations
        const language = req.language || defaultLanguage;

        // Only perform translations when NOT using default language
        if (language !== defaultLanguage) {
          console.log(`Processing translations for language: ${language}`);

          // Count translation tags to see if any exist
          const i18nTagCount = (body.match(/data-i18n=/g) || []).length;
          const i18nAttrCount = (body.match(/data-i18n-attr=/g) || []).length;
          const i18nHtmlCount = (body.match(/data-i18n-html=/g) || []).length;

          console.log(`Found translation tags: ${i18nTagCount} standard, ${i18nAttrCount} attribute, ${i18nHtmlCount} HTML`);

          if (i18nTagCount + i18nAttrCount + i18nHtmlCount > 0) {
            // Process translations...
          } else {
            console.log('No translation tags found in HTML');
          }
        } else {
          console.log(`Using default language (${defaultLanguage}), no translation needed`);
        }

        // Send the modified body
        send(body);
      }
    };
  });
};

