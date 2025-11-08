// utils/translationKeys.js
const crypto = require('crypto');
const { stripBOM } = require('./bomUtils');

/**
 * Generate consistent cache key for translation caching
 * 
 * @param {string} text - Original text to translate
 * @param {string} language - Target language code
 * @param {string} namespace - Translation namespace or context (e.g., 'common', 'rma')
 * @param {object} [options={}] - Additional options affecting the translation
 * @returns {string} - MD5 hash to use as cache key
 */
function generateTranslationKey(text, language, namespace = '', options = {}) {
  // Ensure BOM is stripped before generating the hash key
  const cleanText = stripBOM(text);
  
  // Create base key string
  let keyString = `${cleanText}_${namespace}_${language}`;

  // Add options to key if they affect the translation
  if (options.count !== undefined) {
    keyString += `_count=${options.count}`;
  }
  
  // Add any future option parameters that affect translation output
  // if (options.gender) keyString += `_gender=${options.gender}`;
  // if (options.context) keyString += `_context=${options.context}`;
  
  // Generate consistent MD5 hash
  return crypto.createHash('md5').update(keyString).digest('hex');
}

module.exports = {
  generateTranslationKey
};