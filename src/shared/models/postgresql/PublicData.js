module.exports = (sequelize, DataTypes) => {
  const PublicData = sequelize.define('PublicData', {
    id: {
      type: DataTypes.STRING,
      primaryKey: true,
      allowNull: false
    },
    type: {
      type: DataTypes.STRING,
      allowNull: false
    },
    data: {
      type: DataTypes.JSONB,
      allowNull: false
    }
  }, {
    tableName: 'public_data',
    timestamps: true
  });
  return PublicData;
};
