// models/dict.js
const Betruger = require('./betruger');

/**
 * Dictionary model for translations
 * @class Dict
 * @extends Betruger
 */
class Dict extends Betruger {
  /**
   * Create a new Dictionary entry
   * @param {string} originalText - Original text
   */
  constructor(originalText) {
    super();
    
    this.orig = originalText;
    this.translations = {
      original: originalText
    };
  }
  
  /**
   * Add a translation
   * @param {string} languageCode - Language code (e.g., 'en', 'de', 'fr')
   * @param {string} text - Translated text
   * @returns {boolean} Success status
   */
  addTranslation(languageCode, text) {
    if (!languageCode || !text) return false;
    
    this.translations[languageCode] = text;
    return true;
  }
  
  /**
   * Get translation for a language
   * @param {string} languageCode - Language code
   * @returns {string} Translated text or original if not available
   */
  getTranslation(languageCode) {
    if (!languageCode || !this.translations[languageCode]) {
      return this.translations.original;
    }
    
    return this.translations[languageCode];
  }
  
  /**
   * Get all available translations
   * @returns {Object} Object with language codes and translations
   */
  getAllTranslations() {
    return this.translations;
  }
  
  /**
   * Get available language codes
   * @returns {Array} Array of language codes
   */
  getLanguages() {
    return Object.keys(this.translations);
  }
  
  /**
   * Remove a translation
   * @param {string} languageCode - Language code
   * @returns {boolean} Success status
   */
  removeTranslation(languageCode) {
    if (languageCode === 'original' || !this.translations[languageCode]) {
      return false;
    }
    
    delete this.translations[languageCode];
    return true;
  }
}

module.exports = Dict;