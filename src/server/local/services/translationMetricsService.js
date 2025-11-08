// services/translationMetricsService.js
const { TranslationCache } = require('../../shared/models/postgresql');
const { Sequelize, Op } = require('sequelize');

// Получение статистики по использованию переводов
async function getTranslationUsageStats() {
  try {
    const stats = await TranslationCache.findAll({
      attributes: [
        'language',
        [Sequelize.fn('COUNT', Sequelize.col('key')), 'count'],
        [Sequelize.fn('SUM', Sequelize.col('charCount')), 'totalChars'],
        [Sequelize.fn('AVG', Sequelize.col('processingTime')), 'avgProcessingTime']
      ],
      group: ['language']
    });
    
    return stats.map(stat => ({
      language: stat.language,
      count: parseInt(stat.get('count')),
      totalChars: parseInt(stat.get('totalChars') || 0),
      avgProcessingTime: parseFloat(stat.get('avgProcessingTime') || 0).toFixed(2)
    }));
  } catch (error) {
    console.error('Error getting translation stats:', error);
    return [];
  }
}

// Получение часто используемых переводов для оптимизации
async function getHighUsageTranslations(threshold = 10) {
  try {
    return await TranslationCache.findAll({
      where: {
        useCount: {
          [Op.gte]: threshold
        }
      },
      order: [
        ['useCount', 'DESC']
      ],
      limit: 100
    });
  } catch (error) {
    console.error('Error getting high usage translations:', error);
    return [];
  }
}

// Получение расчётной стоимости использования API
async function getEstimatedApiCosts() {
  try {
    const totalChars = await TranslationCache.sum('charCount', {
      where: {
        source: 'openai'
      }
    });
    
    // Примерный расчёт стоимости (зависит от модели и тарифного плана)
    const estimatedCost = (totalChars / 1000) * 0.002;
    
    return {
      totalChars: totalChars || 0,
      estimatedCost: estimatedCost || 0
    };
  } catch (error) {
    console.error('Error calculating API costs:', error);
    return { totalChars: 0, estimatedCost: 0 };
  }
}

module.exports = {
  getTranslationUsageStats,
  getHighUsageTranslations,
  getEstimatedApiCosts
};