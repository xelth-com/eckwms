// packages/backend/src/services/llm.provider.js
const { GoogleGenAI } = require('@google/genai');

if (!process.env.GEMINI_API_KEY) {
  console.warn('⚠️  WARNING: GEMINI_API_KEY is not configured. LLM features will be disabled.');
}

const genAI = process.env.GEMINI_API_KEY ? new GoogleGenAI(process.env.GEMINI_API_KEY) : null;

/**
 * A shared, singleton instance of the Google AI client.
 */
const geminiClient = genAI;

/**
 * A helper to get a specific model from the shared client.
 * @param {object} options - Model options like modelName.
 * @returns {object} - Model interface for @google/genai SDK
 */
function getGeminiModel(options = {}) {
    if (!genAI) {
        throw new Error('Gemini AI is not available. GEMINI_API_KEY is not configured.');
    }
    
    const modelName = options.modelName || process.env.GEMINI_FALLBACK_MODEL || 'gemini-2.0-flash';
    
    // Return the native SDK interface exactly as in the working version
    return {
        modelName: modelName,
        // Direct access to the native generateContent method
        generateContent: (request) => {
            return genAI.models.generateContent({
                model: modelName,
                contents: request
            });
        }
    };
}

module.exports = {
  geminiClient,
  getGeminiModel
};