module.exports = (sequelize, DataTypes) => {
  return sequelize.define('Place', {
    id: { type: DataTypes.STRING, primaryKey: true },
    data: { type: DataTypes.JSONB }
  }, { tableName: 'places', timestamps: true });
};
