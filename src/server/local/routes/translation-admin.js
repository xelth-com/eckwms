// routes/translation-admin.js [UPDATED VERSION]
const express = require('express');
const router = express.Router();
const path = require('path');
const fs = require('fs');
const { sequelize } = require('../../../shared/models/postgresql');
const { TranslationCache } = require('../../../shared/models/postgresql');
const { translateText, batchTranslate } = require('../services/translationService');
const { requireAdmin } = require('../middleware/auth');
const { stripBOM, parseJSONWithBOM, readJSONWithBOMSync } = require('../utils/bomUtils');

// Получение списка доступных переводов
router.get('/available-translations', requireAdmin, (req, res) => {
  try {
    // FIXED PATH: Using html/locales instead of just locales
    const localesDir = path.join(__dirname, '../locales');
    const languages = fs.readdirSync(localesDir)
      .filter(file => fs.statSync(path.join(localesDir, file)).isDirectory());
    
    const namespaces = fs.readdirSync(path.join(localesDir, 'en'))
      .filter(file => file.endsWith('.json'))
      .map(file => file.replace('.json', ''));
    
    res.json({ languages, namespaces });
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

// Получение содержимого файла перевода
router.get('/translation/:lang/:ns', requireAdmin, (req, res) => {
  try {
    const { lang, ns } = req.params;
    // FIXED PATH: Using html/locales instead of just locales
    const filePath = path.join(__dirname, `../locales/${lang}/${ns}.json`);
    
    if (!fs.existsSync(filePath)) {
      return res.status(404).json({ error: 'Translation file not found' });
    }
    
    // Use BOM-aware JSON reader
    const translations = readJSONWithBOMSync(filePath, fs);
    res.json(translations);
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

// Сохранение переводов
router.post('/translation/:lang/:ns', requireAdmin, (req, res) => {
  try {
    const { lang, ns } = req.params;
    const translations = req.body;
    
    // FIXED PATH: Using html/locales instead of just locales
    const filePath = path.join(__dirname, `../locales/${lang}/${ns}.json`);
    const dirPath = path.dirname(filePath);
    
    // Создаем директорию, если она не существует
    if (!fs.existsSync(dirPath)) {
      fs.mkdirSync(dirPath, { recursive: true });
    }
    
    fs.writeFileSync(filePath, JSON.stringify(translations, null, 2), 'utf8');
    res.json({ success: true });
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

// Получение статистики перевода
router.get('/statistics', requireAdmin, async (req, res) => {
  try {
    // Статистика из файлов
    // FIXED PATH: Using html/locales instead of just locales
    const localesDir = path.join(__dirname, '../locales');
    const languages = fs.readdirSync(localesDir)
      .filter(file => fs.statSync(path.join(localesDir, file)).isDirectory());
    
    const stats = {};
    
    for (const lang of languages) {
      stats[lang] = { namespaces: {}, total: { keys: 0, translated: 0 } };
      
      const namespaces = fs.readdirSync(path.join(localesDir, lang))
        .filter(file => file.endsWith('.json'));
      
      for (const ns of namespaces) {
        const filePath = path.join(localesDir, lang, ns);
        // Use BOM-aware JSON reader
        const content = readJSONWithBOMSync(filePath, fs);
        
        const nsName = ns.replace('.json', '');
        const keys = Object.keys(content);
        const emptyValues = keys.filter(key => !content[key]);
        
        stats[lang].namespaces[nsName] = {
          keys: keys.length,
          translated: keys.length - emptyValues.length,
          progress: ((keys.length - emptyValues.length) / keys.length) * 100
        };
        
        stats[lang].total.keys += keys.length;
        stats[lang].total.translated += (keys.length - emptyValues.length);
      }
      
      if (stats[lang].total.keys > 0) {
        stats[lang].total.progress = 
          (stats[lang].total.translated / stats[lang].total.keys) * 100;
      } else {
        stats[lang].total.progress = 0;
      }
    }
    
    // Статистика из кэша переводов
    const cacheStats = await TranslationCache.findAll({
      attributes: [
        'language',
        [sequelize.fn('COUNT', sequelize.col('key')), 'count']
      ],
      group: ['language']
    });
    
    res.json({ fileStats: stats, cacheStats });
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

// Очистка кэша переводов
router.post('/clear-cache', requireAdmin, async (req, res) => {
  try {
    const { language } = req.body;
    
    let whereClause = {};
    if (language) {
      whereClause = { language };
    }
    
    const deleted = await TranslationCache.destroy({ where: whereClause });
    
    res.json({
      success: true,
      deleted,
      message: language 
        ? `Deleted ${deleted} cached translations for language: ${language}` 
        : `Deleted ${deleted} cached translations`
    });
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

// Автоматический перевод всех отсутствующих строк
router.post('/auto-translate', requireAdmin, async (req, res) => {
  try {
    const { sourceLang, targetLang, namespace } = req.body;
    
    if (!sourceLang || !targetLang || !namespace) {
      return res.status(400).json({ error: 'Missing required parameters' });
    }
    
    // Загружаем исходный файл
    // FIXED PATH: Using html/locales instead of just locales
    const sourceFile = path.join(__dirname, `../locales/${sourceLang}/${namespace}.json`);
    if (!fs.existsSync(sourceFile)) {
      return res.status(404).json({ error: 'Source file not found' });
    }
    
    // Use BOM-aware JSON reader
    const sourceTranslations = readJSONWithBOMSync(sourceFile, fs);
    
    // Загружаем целевой файл (или создаем новый)
    // FIXED PATH: Using html/locales instead of just locales
    const targetFile = path.join(__dirname, `../locales/${targetLang}/${namespace}.json`);
    let targetTranslations = {};
    
    if (fs.existsSync(targetFile)) {
      // Use BOM-aware JSON reader
      targetTranslations = readJSONWithBOMSync(targetFile, fs);
    } else {
      // Создаем директорию, если её нет
      // FIXED PATH: Using html/locales instead of just locales
      const targetDir = path.join(__dirname, `../locales/${targetLang}`);
      if (!fs.existsSync(targetDir)) {
        fs.mkdirSync(targetDir, { recursive: true });
      }
    }
    
    // Находим отсутствующие переводы
    const missing = [];
    const missingKeys = [];
    
    for (const [key, value] of Object.entries(sourceTranslations)) {
      if (!targetTranslations[key] || targetTranslations[key] === '') {
        missing.push(value);
        missingKeys.push(key);
      }
    }
    
    if (missing.length === 0) {
      return res.json({ message: 'No missing translations' });
    }
    
    // Переводим пакетами для экономии API-вызовов
    const BATCH_SIZE = 20;
    const batches = [];
    
    for (let i = 0; i < missing.length; i += BATCH_SIZE) {
      batches.push(missing.slice(i, i + BATCH_SIZE));
    }
    
    let translated = 0;
    const errors = [];
    
    for (let i = 0; i < batches.length; i++) {
      const batch = batches[i];
      const keys = missingKeys.slice(i * BATCH_SIZE, i * BATCH_SIZE + batch.length);
      
      try {
        const result = await batchTranslate(batch, targetLang, namespace);
        
        // Обновляем переводы
        for (let j = 0; j < result.length; j++) {
          targetTranslations[keys[j]] = result[j];
          translated++;
        }
      } catch (error) {
        errors.push(`Batch ${i + 1}: ${error.message}`);
      }
      
      // Периодически сохраняем, чтобы не потерять прогресс
      if (i % 5 === 0 || i === batches.length - 1) {
        fs.writeFileSync(targetFile, JSON.stringify(targetTranslations, null, 2), 'utf8');
      }
    }
    
    // Сохраняем окончательный результат
    fs.writeFileSync(targetFile, JSON.stringify(targetTranslations, null, 2), 'utf8');
    
    res.json({
      success: true,
      translated,
      errors,
      message: `Translated ${translated} out of ${missing.length} missing strings.`
    });
  } catch (error) {
    res.status(500).json({ error: error.message });
  }
});

module.exports = router;