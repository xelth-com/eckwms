module.exports = (sequelize, DataTypes) => {
  const Role = sequelize.define('Role', {
    id: {
      type: DataTypes.INTEGER,
      primaryKey: true,
      autoIncrement: true
    },
    name: {
      type: DataTypes.STRING,
      allowNull: false,
      unique: true
    },
    description: {
      type: DataTypes.TEXT,
      allowNull: true
    },
    is_system_protected: {
      type: DataTypes.BOOLEAN,
      defaultValue: false,
      comment: 'If true, this role cannot be modified by the Agent'
    }
  }, {
    tableName: 'roles',
    timestamps: true
  });
  return Role;
};
