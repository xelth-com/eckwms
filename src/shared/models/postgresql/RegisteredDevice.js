module.exports = (sequelize, DataTypes) => {
  const RegisteredDevice = sequelize.define('RegisteredDevice', {
    deviceId: {
      type: DataTypes.STRING,
      primaryKey: true
    },
    instance_id: {
      type: DataTypes.UUID,
      allowNull: false,
      references: {
        model: 'eckwms_instances',
        key: 'id'
      }
    },
    is_active: {
      type: DataTypes.BOOLEAN,
      defaultValue: true
    },
    deviceName: {
      type: DataTypes.STRING,
      allowNull: true
    }
  }, {
    timestamps: true,
    tableName: 'registered_devices'
  });
  return RegisteredDevice;
};
