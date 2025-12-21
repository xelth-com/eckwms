# Gemini AI Services Documentation

## Overview

eckWMS now includes Google Gemini AI services, adapted from the proven ecKasse architecture.

## Installed Services

### 1. LLM Provider (`llm.provider.js`)
**Purpose:** Singleton client for Google Gemini API

```javascript
const { geminiClient, getGeminiModel } = require('./services/llm.provider');

// Check if available
if (geminiClient) {
  // Ready to use
}

// Get specific model
const model = getGeminiModel({ modelName: 'gemini-2.5-flash' });
```

### 2. Gemini Service (`geminiService.js`)
**Purpose:** High-level service for text generation and chat

```javascript
const geminiService = require('./services/geminiService');

// Simple text generation
const response = await geminiService.generateText('Describe this product: Laptop');

// Chat with history
const chat = await geminiService.chat('What is this?', chatHistory);
console.log(chat.text);
console.log(chat.history); // Updated history

// Simple query (for enrichment, classification)
const result = await geminiService.invokeSimpleQuery('Classify this: Electronics');
```

### 3. Embedding Service (`embeddingService.js`)
**Purpose:** Generate vector embeddings for semantic search

```javascript
const { generateEmbedding, generateBatchEmbeddings } = require('./services/embeddingService');

// Single embedding
const vector = await generateEmbedding('Wireless Mouse');
// Returns: [0.123, -0.456, ...] // 768 dimensions

// Batch embeddings
const vectors = await generateBatchEmbeddings([
  'Keyboard',
  'Monitor',
  'Headphones'
]);
// Returns: [[...], [...], [...]]

// For database storage
const { embeddingToBuffer, bufferToEmbedding } = require('./services/embeddingService');
const buffer = embeddingToBuffer(vector);
// Store buffer in PostgreSQL or SQLite
```

### 4. Google Sheets Service (`googleSheetsService.js`)
**Purpose:** Export data to Google Sheets

```javascript
const googleSheetsService = require('./services/googleSheetsService');

// Append row
await googleSheetsService.appendToSheet([
  '2025-12-21',
  'Product scanned',
  'Barcode: 123456',
  'User: John'
]);
```

### 5. Google Sheets Tool (`googleSheetsTool.js`)
**Purpose:** AI tool for dynamic Google Sheets interactions

```javascript
const googleSheetsTool = require('./tools/googleSheetsTool');

// Use in AI function calling
const result = await googleSheetsTool.execute({
  action: 'append_row',
  data: ['Timestamp', 'Event', 'Details']
});
```

### 6. Error Handler (`geminiErrorHandler.js`)
**Purpose:** Robust error handling for Gemini API

```javascript
const { handleGeminiError, createGeminiErrorLog } = require('./utils/geminiErrorHandler');

try {
  const result = await geminiService.generateText('...');
} catch (error) {
  const errorInfo = handleGeminiError(error, {
    language: 'en',
    includeRetryInfo: true
  });

  console.log(errorInfo.userMessage); // User-friendly message
  console.log(errorInfo.isTemporary); // Should retry?
  console.log(errorInfo.retryDelay); // Wait time in seconds

  const log = createGeminiErrorLog(error, {
    operation: 'scan_enrichment',
    userId: '123'
  });
}
```

## Environment Variables

Required in `.env`:

```bash
# Gemini API Key (required)
GEMINI_API_KEY=AIzaSyB2SU8Ox6l9FHhzMJwRuEiTx-UXEoN03h4

# Model selection
GEMINI_PRIMARY_MODEL=gemini-2.5-flash
GEMINI_FALLBACK_MODEL=gemini-2.0-flash
GEMINI_EMBEDDING_MODEL=gemini-embedding-001

# Google Sheets (optional)
GOOGLE_SPREADSHEET_ID=your_spreadsheet_id_here

# Google Custom Search (optional)
GCS_API_KEY=AIzaSyB2SU8Ox6l9FHhzMJwRuEiTx-UXEoN03h4
GCS_CX=YOUR_SEARCH_ENGINE_ID
```

## Use Cases in eckWMS

### 1. Product Enrichment
```javascript
const geminiService = require('./services/geminiService');

async function enrichProduct(barcode, name) {
  const prompt = `Product: ${name}\nBarcode: ${barcode}\n\nGenerate a detailed description and suggest a category.`;
  const response = await geminiService.generateText(prompt);
  return response;
}
```

### 2. Smart Search
```javascript
const { generateEmbedding } = require('./services/embeddingService');

async function semanticSearch(query) {
  // Generate query embedding
  const queryVector = await generateEmbedding(query);

  // Search in database (PostgreSQL with pgvector or SQLite with sqlite-vec)
  const results = await db.raw(`
    SELECT *, embedding <-> ? AS distance
    FROM products
    ORDER BY distance
    LIMIT 10
  `, [queryVector]);

  return results;
}
```

### 3. Scan Analysis
```javascript
async function analyzeScan(scannedData) {
  const prompt = `Analyze this scanned data and extract product info:\n${scannedData}`;
  const result = await geminiService.invokeSimpleQuery(prompt);
  return JSON.parse(result);
}
```

### 4. Intelligent Logging
```javascript
const googleSheetsTool = require('./tools/googleSheetsTool');

async function logImportantEvent(event) {
  // AI decides what to log
  const analysis = await geminiService.generateText(
    `Should this event be logged to manager sheet? ${event.type}: ${event.details}`
  );

  if (analysis.includes('yes')) {
    await googleSheetsTool.execute({
      action: 'append_row',
      data: [new Date().toISOString(), event.type, event.details]
    });
  }
}
```

## Error Handling Best Practices

```javascript
const { handleGeminiError } = require('./utils/geminiErrorHandler');

async function robustAICall(prompt) {
  try {
    return await geminiService.generateText(prompt);
  } catch (error) {
    const errorInfo = handleGeminiError(error);

    if (errorInfo.isTemporary) {
      // Temporary error - can retry
      console.log(`Retry in ${errorInfo.retryDelay}s`);
      await sleep(errorInfo.retryDelay * 1000);
      return await robustAICall(prompt); // Retry
    } else {
      // Permanent error - fallback
      console.error(errorInfo.userMessage);
      return getFallbackResponse(prompt);
    }
  }
}
```

## Rate Limits & Costs

**Gemini Flash (Free Tier):**
- 15 requests/minute
- 1,500 requests/day
- 1 million tokens/day

**Gemini Embeddings (Free Tier):**
- 1,500 requests/day

**Best Practices:**
1. Cache embeddings in database
2. Batch similar requests
3. Use fallback models
4. Implement exponential backoff
5. Monitor usage via error handler

## Testing

```javascript
// Test Gemini connection
const geminiService = require('./services/geminiService');

async function testGemini() {
  if (!geminiService.isAvailable()) {
    console.error('❌ Gemini not available - check GEMINI_API_KEY');
    return;
  }

  const response = await geminiService.generateText('Hello, respond with OK if working');
  console.log('✅ Gemini response:', response);
}

testGemini();
```

## Migration from Old SDK

If you had `@google/generative-ai`, it's been replaced with `@google/genai`:

**Old:**
```javascript
const { GoogleGenerativeAI } = require('@google/generative-ai');
const genAI = new GoogleGenerativeAI(apiKey);
const model = genAI.getGenerativeModel({ model: 'gemini-pro' });
```

**New:**
```javascript
const { geminiClient } = require('./services/llm.provider');
// Use geminiClient.models.generateContent({ ... })
```

## Architecture from ecKasse

This implementation is battle-tested from ecKasse (POS system):
- ✅ Production-ready error handling
- ✅ Automatic model fallback
- ✅ Multilingual support
- ✅ Function calling (tools)
- ✅ Conversation management
- ✅ Rate limit handling

## Next Steps

1. **Implement product enrichment** using Gemini
2. **Add semantic search** with embeddings
3. **Create AI assistant** for warehouse operations
4. **Build smart categorization** for new products
5. **Integrate with ecKasse** for unified commerce solution

---

*Based on ecKasse architecture - Proven in production*
