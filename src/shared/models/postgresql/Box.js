module.exports = (sequelize, DataTypes) => {
  return sequelize.define('Box', {
    id: { type: DataTypes.STRING, primaryKey: true },
    data: { type: DataTypes.JSONB }
  }, { tableName: 'boxes', timestamps: true });
};
