module.exports = (sequelize, DataTypes) => {
  return sequelize.define('Item', {
    id: { type: DataTypes.STRING, primaryKey: true },
    class: { type: DataTypes.STRING },
    data: { type: DataTypes.JSONB }
  }, { tableName: 'items', timestamps: true });
};
