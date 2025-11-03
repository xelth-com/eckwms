module.exports = (sequelize, DataTypes) => {
  const EckwmsInstance = sequelize.define('EckwmsInstance', {
    id: {
      type: DataTypes.UUID,
      defaultValue: DataTypes.UUIDV4,
      primaryKey: true
    },
    name: {
      type: DataTypes.STRING,
      allowNull: false,
      unique: true
    },
    server_url: {
      type: DataTypes.STRING,
      allowNull: false
    },
    api_key: {
      type: DataTypes.STRING,
      allowNull: false,
      unique: true
    },
    tier: {
      type: DataTypes.ENUM('free', 'paid'),
      defaultValue: 'free',
      allowNull: false
    }
  }, {
    timestamps: true,
    tableName: 'eckwms_instances'
  });
  return EckwmsInstance;
};
