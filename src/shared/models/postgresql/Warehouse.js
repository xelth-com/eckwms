module.exports = (sequelize, DataTypes) => {
  const Warehouse = sequelize.define('Warehouse', {
    id: {
      type: DataTypes.INTEGER,
      primaryKey: true,
      autoIncrement: true
    },
    name: {
      type: DataTypes.STRING(100),
      allowNull: false,
      comment: 'Warehouse name (e.g. Main Warehouse, Area 51)'
    },
    id_offset: {
      type: DataTypes.INTEGER,
      allowNull: false,
      defaultValue: 0,
      comment: 'ID offset for this warehouse (e.g. 0, 10000000, 20000000)'
    },
    is_active: {
      type: DataTypes.BOOLEAN,
      allowNull: false,
      defaultValue: true,
      comment: 'Whether this warehouse is currently active'
    }
  }, {
    tableName: 'warehouses',
    timestamps: true,
    indexes: [
      { fields: ['is_active'] },
      { fields: ['id_offset'], unique: true }
    ]
  });
  return Warehouse;
};
