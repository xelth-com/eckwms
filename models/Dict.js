// models/Dict.js
class Dict {
    constructor(originalText) {
        this.translations = { original: originalText }; // Originaltext wird gespeichert
    }

    // Fügt eine Übersetzung hinzu
    addTranslation(langCode, translatedText) {
        this.translations[langCode] = translatedText;
    }

    // Holt eine Übersetzung, falls sie existiert
    getTranslation(langCode) {
        return this.translations[langCode] || `No translation available for '${langCode}'`;
    }

    // Überprüft, ob eine Sprache vorhanden ist
    hasTranslation(langCode) {
        return !!this.translations[langCode];
    }

    // JSON-Export für Speicherung
    toJSON() {
        return this.translations;
    }
}

module.exports = Dict;