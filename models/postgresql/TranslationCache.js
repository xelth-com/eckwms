// models/postgresql/TranslationCache.js
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
    }
  }, {
    timestamps: true,
    indexes: [
      {
        fields: ['language', 'key']
      },
      {
        fields: ['lastUsed']
      },
      {
        fields: ['useCount']
      }
    ],
    tableName: 'translation_cache'
  });

  return TranslationCache;
};