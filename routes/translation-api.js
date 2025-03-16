// routes/translation-api.js
const express = require('express');
const router = express.Router();
const { translateText, batchTranslate } = require('../services/translationService');
const { optionalAuth } = require('../middleware/auth');

// Применяем опциональную аутентификацию для всех маршрутов
router.use(optionalAuth);

/**
 * Маршрут для перевода одного текста
 * POST /api/translate
 * Body: { text: string, targetLang: string, context: string }
 */
router.post('/translate', async (req, res) => {
  try {
    const { text, targetLang, context, sourceLang } = req.body;
    
    if (!text || !targetLang) {
      return res.status(400).json({ error: 'Text and target language required' });
    }

    const translatedText = await translateText(
      text, 
      targetLang, 
      context || '', 
      sourceLang || 'de'
    );
    
    res.json({ 
      original: text, 
      translated: translatedText, 
      language: targetLang 
    });
  } catch (error) {
    console.error("Translation error:", error);
    res.status(500).json({ error: "Translation failed", details: error.message });
  }
});

/**
 * Маршрут для пакетного перевода текстов
 * POST /api/translate-batch
 * Body: { texts: string[], targetLang: string, context: string }
 */
router.post('/translate-batch', async (req, res) => {
  try {
    const { texts, targetLang, context, sourceLang } = req.body;
    
    if (!texts || !Array.isArray(texts) || !targetLang) {
      return res.status(400).json({ error: 'Array of texts and target language required' });
    }

    const translations = await batchTranslate(
      texts, 
      targetLang, 
      context || '', 
      sourceLang || 'de'
    );
    
    res.json({ 
      count: translations.length,
      translations 
    });
  } catch (error) {
    console.error("Batch translation error:", error);
    res.status(500).json({ error: "Batch translation failed", details: error.message });
  }
});

/**
 * Маршрут для детектирования языка (для future use)
 * POST /api/detect-language
 * Body: { text: string }
 */
router.post('/detect-language', async (req, res) => {
  try {
    const { text } = req.body;
    
    if (!text) {
      return res.status(400).json({ error: 'Text required' });
    }
    
    // Здесь в будущем может быть реализована функция детектирования языка
    // Пока отправляем упрощенный ответ
    
    res.json({ 
      text: text,
      detectedLanguage: 'auto',
      message: 'Language detection not implemented yet'
    });
  } catch (error) {
    console.error("Language detection error:", error);
    res.status(500).json({ error: "Language detection failed", details: error.message });
  }
});

module.exports = router;
