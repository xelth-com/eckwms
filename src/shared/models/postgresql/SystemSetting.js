module.exports = (sequelize, DataTypes) => {
  const SystemSetting = sequelize.define('SystemSetting', {
    id: {
      type: DataTypes.INTEGER,
      primaryKey: true,
      autoIncrement: true
    },
    key: {
      type: DataTypes.STRING(100),
      allowNull: false,
      unique: true
    },
    value: {
      type: DataTypes.TEXT,
      allowNull: false
    },
    description: {
      type: DataTypes.TEXT,
      allowNull: true
    }
  }, {
    tableName: 'system_settings',
    timestamps: true,
    indexes: [
      { unique: true, fields: ['key'] }
    ]
  });

  // Helper method to get a setting value
  SystemSetting.getValue = async function(key, defaultValue = null) {
    const setting = await this.findOne({ where: { key } });
    return setting ? setting.value : defaultValue;
  };

  // Helper method to set a setting value
  SystemSetting.setValue = async function(key, value, description = null) {
    const [setting, created] = await this.findOrCreate({
      where: { key },
      defaults: { value, description }
    });

    if (!created) {
      setting.value = value;
      if (description !== null) {
        setting.description = description;
      }
      await setting.save();
    }

    return setting;
  };

  // Helper method to increment a counter and return the new value
  SystemSetting.incrementCounter = async function(key, incrementBy = 1) {
    const setting = await this.findOne({ where: { key } });
    if (!setting) {
      throw new Error(`Counter setting '${key}' not found`);
    }

    const currentValue = parseInt(setting.value, 10);
    const newValue = currentValue + incrementBy;
    setting.value = newValue.toString();
    await setting.save();

    return newValue;
  };

  return SystemSetting;
};
