// middleware/htmlInterceptor.js
const interceptor = require('express-interceptor');

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
        console.log('Body length:', body.length);

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
            // Process translations...
            // This is a placeholder for your translation processing code
            // If you had code here in the original file, you should restore it
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