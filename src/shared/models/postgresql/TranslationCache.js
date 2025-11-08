// models/postgresql/TranslationCache.js
// Update the model definition with optimized indexes

module.exports = (sequelize, DataTypes) => {
  const TranslationCache = sequelize.define('TranslationCache', {
    key: {
      type: DataTypes.STRING(32),
      primaryKey: true
    },
    language: {
      type: DataTypes.STRING(5),
      primaryKey: true
    },
    originalText: {
      type: DataTypes.TEXT,
      allowNull: false
    },
    translatedText: {
      type: DataTypes.TEXT,
      allowNull: false
    },
    context: {
      type: DataTypes.STRING(100),
      allowNull: true
    },
    lastUsed: {
      type: DataTypes.DATE,
      defaultValue: DataTypes.NOW
    },
    useCount: {
      type: DataTypes.INTEGER,
      defaultValue: 1
    },
    // Add these new fields for better metrics
    charCount: {
      type: DataTypes.INTEGER,
      defaultValue: 0
    },
    processingTime: {
      type: DataTypes.FLOAT,
      allowNull: true
    },
    source: {
      type: DataTypes.STRING(20),
      defaultValue: 'openai'
    },
    apiVersion: {
      type: DataTypes.STRING(30),
      allowNull: true
    }
  }, {
    timestamps: true,
    indexes: [
      // Optimize existing indexes
      {
        fields: ['language', 'key'],
        unique: true  // Add uniqueness constraint
      },
      // Add indexes for common queries
      {
        fields: ['language', 'lastUsed']
      },
      {
        fields: ['useCount'],
        // For high-usage translation identification
      },
      {
        fields: ['source', 'createdAt'],
        // For API usage tracking
      },
      // Add a functional index for substring matching if PostgreSQL version supports it
      // This needs to be added via migration or custom SQL for most databases
      // sequelize.literal('CREATE INDEX idx_original_text_trgm ON translation_cache USING gin (original_text gin_trgm_ops)')
    ],
    tableName: 'translation_cache'
  });

  return TranslationCache;
};