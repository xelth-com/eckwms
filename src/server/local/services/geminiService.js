// Gemini Service for eckWMS
// Based on ecKasse architecture, adapted for warehouse management

const { geminiClient } = require('./llm.provider');
const { handleGeminiError, createGeminiErrorLog } = require('../utils/geminiErrorHandler');

class GeminiService {
    constructor() {
        this.client = geminiClient;
        this.primaryModel = process.env.GEMINI_PRIMARY_MODEL || 'gemini-2.5-flash';
        this.fallbackModel = process.env.GEMINI_FALLBACK_MODEL || 'gemini-2.0-flash';
    }

    /**
     * Check if Gemini is available
     */
    isAvailable() {
        return !!this.client;
    }

    /**
     * Get prioritized models for fallback
     */
    getPrioritizedModels() {
        return [
            { name: this.primaryModel, temperature: 0.1 },
            { name: this.fallbackModel, temperature: 0.1 }
        ];
    }

    /**
     * Simple text generation without tools
     * @param {string} prompt - The prompt to send
     * @param {Object} options - Additional options
     * @returns {Promise<string>} - Generated text
     */
    async generateText(prompt, options = {}) {
        if (!this.isAvailable()) {
            console.warn('[Gemini] Service not available - GEMINI_API_KEY not configured');
            return 'AI Service not configured';
        }

        const models = this.getPrioritizedModels();

        for (const [index, modelConfig] of models.entries()) {
            try {
                const result = await this.client.models.generateContent({
                    model: modelConfig.name,
                    systemInstruction: options.systemInstruction || "You are a helpful AI assistant for warehouse management.",
                    generationConfig: {
                        temperature: options.temperature || modelConfig.temperature
                    },
                    contents: [{ role: 'user', parts: [{ text: prompt }] }]
                });

                if (!result.candidates || result.candidates.length === 0) {
                    throw new Error('No candidates in response');
                }

                const content = result.candidates[0].content;
                return content.parts
                    .filter(part => part.text)
                    .map(part => part.text)
                    .join('');

            } catch (error) {
                console.error(`[Gemini] Model ${modelConfig.name} failed:`, error.message);

                // If last model, handle error properly
                if (index === models.length - 1) {
                    const geminiErrorInfo = handleGeminiError(error, {
                        language: 'en',
                        includeRetryInfo: true
                    });

                    const errorLog = createGeminiErrorLog(error, {
                        operation: 'text_generation',
                        prompt: prompt.substring(0, 100)
                    });

                    if (errorLog.level === 'warn') {
                        console.warn(errorLog);
                    } else {
                        console.error(errorLog);
                    }

                    return `Error: ${geminiErrorInfo.userMessage}`;
                }
            }
        }

        return 'Failed to generate text';
    }

    /**
     * Generate text with conversation history
     * @param {string} userMessage - The user's message
     * @param {Array} chatHistory - Previous conversation history
     * @param {Object} options - Additional options
     * @returns {Promise<Object>} - Object with text and updated history
     */
    async chat(userMessage, chatHistory = [], options = {}) {
        if (!this.isAvailable()) {
            return {
                text: 'AI Service not configured',
                history: chatHistory
            };
        }

        const models = this.getPrioritizedModels();

        // Convert chat history to Gemini format
        const history = chatHistory.map(msg => ({
            role: msg.role === 'user' ? 'user' : 'model',
            parts: [{ text: msg.text || msg.content }]
        }));

        for (const [index, modelConfig] of models.entries()) {
            try {
                const result = await this.client.models.generateContent({
                    model: modelConfig.name,
                    systemInstruction: options.systemInstruction || "You are a helpful AI assistant for warehouse management.",
                    contents: [
                        ...history,
                        { role: 'user', parts: [{ text: userMessage }] }
                    ],
                    config: {
                        generationConfig: {
                            temperature: options.temperature || modelConfig.temperature
                        }
                    }
                });

                if (!result.candidates || result.candidates.length === 0) {
                    throw new Error('No candidates in response');
                }

                const content = result.candidates[0].content;
                const responseText = content.parts
                    .filter(part => part.text)
                    .map(part => part.text)
                    .join('');

                const newHistory = [
                    ...chatHistory,
                    { role: 'user', text: userMessage },
                    { role: 'model', text: responseText }
                ];

                return { text: responseText, history: newHistory };

            } catch (error) {
                console.error(`[Gemini] Chat model ${modelConfig.name} failed:`, error.message);

                if (index === models.length - 1) {
                    const geminiErrorInfo = handleGeminiError(error, {
                        language: 'en',
                        includeRetryInfo: true
                    });

                    return {
                        text: geminiErrorInfo.userMessage,
                        history: chatHistory,
                        error: true
                    };
                }
            }
        }

        return {
            text: 'Failed to generate response',
            history: chatHistory,
            error: true
        };
    }

    /**
     * Simple query for programmatic calls (like enrichment, classification)
     * @param {string} promptText - The prompt
     * @returns {Promise<string>} - Response text
     */
    async invokeSimpleQuery(promptText) {
        return this.generateText(promptText, {
            systemInstruction: "You are a helpful assistant that responds accurately and concisely. If the user asks for JSON, provide only the valid JSON object and nothing else."
        });
    }
}

module.exports = new GeminiService();
