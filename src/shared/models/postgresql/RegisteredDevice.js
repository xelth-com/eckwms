module.exports = (sequelize, DataTypes) => {
  const RegisteredDevice = sequelize.define('RegisteredDevice', {
    deviceId: {
      type: DataTypes.STRING,
      primaryKey: true
    },
    instance_id: {
      type: DataTypes.UUID,
      allowNull: true,  // Made optional for standalone instances
      references: {
        model: 'eckwms_instances',
        key: 'id'
      }
    },
    is_active: {
      type: DataTypes.BOOLEAN,
      defaultValue: true
    },
    publicKey: {
      type: DataTypes.TEXT,
      allowNull: false,
      comment: 'Base64-encoded Ed25519 public key for this device'
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
