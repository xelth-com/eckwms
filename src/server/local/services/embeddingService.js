// File: /packages/backend/src/services/embedding.service.js

const { geminiClient: ai } = require('./llm.provider');
const { handleGeminiError, createGeminiErrorLog } = require('../utils/geminiErrorHandler');

// Отключаем моки - используем только реальный API
const USE_MOCK_EMBEDDINGS = false;

/**
 * Generate embedding vector for text using Google's gemini-embedding-001 model
 * @param {string} text - Text to generate embedding for
 * @param {Object} options - Additional options
 * @returns {Promise<number[]>} - Array of 768 float values representing the embedding
 */
async function generateEmbedding(text, options = {}) {
  
  try {
    console.log(`🔍 Генерирую embedding для: "${text.substring(0, 50)}${text.length > 50 ? '...' : ''}"`);
    
    const response = await ai.models.embedContent({
      model: options.model || process.env.GEMINI_EMBEDDING_MODEL || 'gemini-embedding-001',
      contents: [text],
      config: {
        taskType: options.taskType || "RETRIEVAL_DOCUMENT",
        outputDimensionality: options.outputDimensionality || 768
      }
    });
    
    // Правильное извлечение values из структуры ответа
    if (!response.embeddings || response.embeddings.length === 0) {
      throw new Error('API returned no embeddings.');
    }
    
    const embeddingObject = response.embeddings[0];
    const embedding = embeddingObject.values;
    const stats = embeddingObject.statistics;
    
    // The `statistics` object is optional
    if (stats && typeof stats.token_count !== 'undefined') {
      console.log(`✅ Embedding создан: ${embedding.length} измерений, ${stats.token_count} токенов`);
      if (stats.truncated) {
        console.warn('⚠️  Текст был обрезан при создании embedding');
      }
    } else {
      console.log(`✅ Embedding создан: ${embedding.length} измерений (статистика токенов недоступна)`);
    }
    
    return embedding;
    
  } catch (error) {
    // Обработка специфических ошибок Gemini API
    const geminiErrorInfo = handleGeminiError(error, { 
      language: 'ru', 
      includeRetryInfo: true 
    });
    
    // Создаем структурированный лог
    const errorLog = createGeminiErrorLog(error, {
      operation: 'embedding_generation',
      text: text.substring(0, 50), // Первые 50 символов текста
      isTemporary: geminiErrorInfo.isTemporary
    });
    
    // Выводим лог в консоль с соответствующим уровнем
    if (errorLog.level === 'warn') {
      console.warn('🚦 GEMINI EMBEDDING LIMIT:', errorLog.userMessage);
      console.warn('   Retry in:', errorLog.retryDelay + 's');
    } else {
      console.error('❌ GEMINI EMBEDDING ERROR:', errorLog.userMessage);
      console.error('   This will cause item import to FAIL!');
    }
    
    throw error;
  }
}

/**
 * Generate embeddings for multiple texts at once
 * @param {string[]} texts - Array of texts to generate embeddings for
 * @param {Object} options - Additional options
 * @returns {Promise<number[][]>} - Array of embedding vectors
 */
async function generateBatchEmbeddings(texts, options = {}) {
  
  try {
    console.log(`🔍 Генерирую batch embeddings для ${texts.length} текстов`);
    
    const response = await ai.models.embedContent({
      model: options.model || process.env.GEMINI_EMBEDDING_MODEL || 'gemini-embedding-001',
      contents: texts,
      config: {
        taskType: options.taskType || "RETRIEVAL_DOCUMENT",
        outputDimensionality: options.outputDimensionality || 768
      }
    });
    
    // Извлекаем values из каждого embedding
    const embeddings = response.embeddings.map(embedding => embedding.values);
    const totalTokens = response.embeddings.reduce((sum, emb) => {
      return sum + (emb.statistics?.token_count || 0);
    }, 0);
    
    console.log(`✅ Batch embeddings созданы: ${embeddings.length} векторов, ${totalTokens} токенов`);
    
    return embeddings;
    
  } catch (error) {
    const geminiErrorInfo = handleGeminiError(error, { 
      language: 'ru', 
      includeRetryInfo: true 
    });
    
    console.error('❌ GEMINI BATCH EMBEDDING ERROR:', geminiErrorInfo.userMessage);
    throw error;
  }
}

/**
 * Get embedding statistics for text
 * @param {string} text - Text to analyze
 * @param {Object} options - Additional options
 * @returns {Promise<Object>} - Statistics object
 */
async function getEmbeddingStats(text, options = {}) {
  
  try {
    const response = await ai.models.embedContent({
      model: options.model || process.env.GEMINI_EMBEDDING_MODEL || 'gemini-embedding-001',
      contents: [text],
      config: {
        taskType: options.taskType || "RETRIEVAL_DOCUMENT",
        outputDimensionality: options.outputDimensionality || 768
      }
    });
    
    const embedding = response.embeddings[0];
    
    return {
      dimensions: embedding.values.length,
      tokenCount: embedding.statistics?.token_count ?? 0,
      truncated: embedding.statistics?.truncated ?? false,
      billableCharacters: response.metadata?.billable_character_count || 0
    };
    
  } catch (error) {
    console.error('❌ Error getting embedding stats:', error.message);
    throw error;
  }
}

/**
 * Convert embedding array to Float32Array buffer for sqlite-vec
 * @param {number[]} embedding - Array of float values
 * @returns {Buffer} - Buffer suitable for sqlite-vec
 */
function embeddingToBuffer(embedding) {
  const float32Array = new Float32Array(embedding);
  return Buffer.from(float32Array.buffer);
}

/**
 * Convert Buffer back to regular array
 * @param {Buffer} buffer - Buffer from sqlite-vec
 * @returns {number[]} - Array of float values
 */
function bufferToEmbedding(buffer) {
  const float32Array = new Float32Array(buffer.buffer, buffer.byteOffset, buffer.length / 4);
  return Array.from(float32Array);
}

/**
 * Convert embedding array to JSON string (deprecated - for compatibility)
 * @param {number[]} embedding - Array of float values
 * @returns {string} - JSON string
 */
function embeddingToJson(embedding) {
  return JSON.stringify(embedding);
}

/**
 * Convert string back to regular array - handles both JSON array and PostgreSQL array formats
 * @param {string} stringData - JSON array string or PostgreSQL array string
 * @returns {number[]} - Array of float values
 */
function jsonToEmbedding(stringData) {
  // Handle PostgreSQL array format like {"0.1","0.2","0.3"}
  if (stringData.startsWith('{') && stringData.endsWith('}') && !stringData.startsWith('[')) {
    const vectorStr = stringData.replace(/^\{|\}$/g, ''); // Remove curly braces
    return vectorStr.split(',').map(v => parseFloat(v.replace(/"/g, ''))); // Split and parse floats
  }
  // Handle JSON array format like [0.1,0.2,0.3]
  return JSON.parse(stringData);
}

module.exports = {
  generateEmbedding,
  generateBatchEmbeddings,
  getEmbeddingStats,
  embeddingToBuffer,
  bufferToEmbedding,
  embeddingToJson,
  jsonToEmbedding
};