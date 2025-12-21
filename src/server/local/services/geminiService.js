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

    /**
     * Generate with function calling support
     * @param {string} userMessage - The user's message
     * @param {Array} tools - Array of tool definitions with {name, description, parameters, execute}
     * @param {Object} options - Additional options (systemInstruction, temperature, etc.)
     * @returns {Promise<Object>} - Object with text, toolCalls, and execution results
     */
    async generateWithTools(userMessage, tools = [], options = {}) {
        if (!this.isAvailable()) {
            return {
                text: 'AI Service not configured',
                error: true
            };
        }

        const models = this.getPrioritizedModels();

        // Convert tools to Gemini format
        const functionDeclarations = tools.map(tool => ({
            name: tool.name,
            description: tool.description,
            parameters: tool.parameters
        }));

        for (const [index, modelConfig] of models.entries()) {
            try {
                let contents = [{ role: 'user', parts: [{ text: userMessage }] }];
                let maxIterations = 5; // Prevent infinite loops
                let iteration = 0;
                let finalResponse = null;

                while (iteration < maxIterations) {
                    iteration++;

                    const requestConfig = {
                        model: modelConfig.name,
                        systemInstruction: options.systemInstruction || "You are a helpful AI assistant.",
                        contents: contents,
                        generationConfig: {
                            temperature: options.temperature || modelConfig.temperature
                        }
                    };

                    // Add tools if provided
                    if (functionDeclarations.length > 0) {
                        requestConfig.tools = [{
                            functionDeclarations: functionDeclarations
                        }];
                    }

                    const result = await this.client.models.generateContent(requestConfig);

                    if (!result.candidates || result.candidates.length === 0) {
                        throw new Error('No candidates in response');
                    }

                    const candidate = result.candidates[0];
                    const content = candidate.content;

                    // Check if the response contains function calls
                    const functionCalls = content.parts.filter(part => part.functionCall);

                    if (functionCalls.length > 0) {
                        // Execute function calls
                        const functionResponses = [];

                        for (const fc of functionCalls) {
                            const toolName = fc.functionCall.name;
                            const toolArgs = fc.functionCall.args;
                            const tool = tools.find(t => t.name === toolName);

                            console.log(`[Gemini] Tool call: ${toolName} with args:`, toolArgs);

                            let functionResponse;
                            if (tool && tool.execute) {
                                try {
                                    const executionResult = await tool.execute(toolArgs);
                                    functionResponse = {
                                        name: toolName,
                                        response: executionResult
                                    };
                                } catch (execError) {
                                    console.error(`[Gemini] Tool execution error:`, execError);
                                    functionResponse = {
                                        name: toolName,
                                        response: { error: execError.message }
                                    };
                                }
                            } else {
                                functionResponse = {
                                    name: toolName,
                                    response: { error: 'Tool not found' }
                                };
                            }

                            functionResponses.push(functionResponse);
                        }

                        // Add assistant's function call to history
                        contents.push({
                            role: 'model',
                            parts: functionCalls.map(fc => ({ functionCall: fc.functionCall }))
                        });

                        // Add function responses to history
                        contents.push({
                            role: 'user',
                            parts: functionResponses.map(fr => ({
                                functionResponse: fr
                            }))
                        });

                        // Continue the loop to get AI's next response
                        continue;
                    }

                    // No function calls, extract text response
                    const textParts = content.parts.filter(part => part.text);
                    if (textParts.length > 0) {
                        finalResponse = textParts.map(part => part.text).join('');
                        break;
                    }

                    // If we get here with no text and no function calls, something's wrong
                    throw new Error('Response contained neither text nor function calls');
                }

                if (!finalResponse) {
                    throw new Error('Max iterations reached without final response');
                }

                return {
                    text: finalResponse,
                    iterations: iteration
                };

            } catch (error) {
                console.error(`[Gemini] Tool model ${modelConfig.name} failed:`, error.message);

                if (index === models.length - 1) {
                    const geminiErrorInfo = handleGeminiError(error, {
                        language: 'en',
                        includeRetryInfo: true
                    });

                    return {
                        text: geminiErrorInfo.userMessage,
                        error: true
                    };
                }
            }
        }

        return {
            text: 'Failed to generate response with tools',
            error: true
        };
    }
}

module.exports = new GeminiService();
