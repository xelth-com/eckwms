// utils/i18nStorage.js
const { AsyncLocalStorage } = require('async_hooks');

// Create a single shared instance
const i18nStorage = new AsyncLocalStorage();

module.exports = { i18nStorage };