module.exports = (sequelize, DataTypes) => {
  const WarehouseRack = sequelize.define('WarehouseRack', {
    id: {
      type: DataTypes.INTEGER,
      primaryKey: true,
      autoIncrement: true
    },
    name: {
      type: DataTypes.STRING(100),
      allowNull: false,
      comment: 'Human readable name (e.g. Aisle A)'
    },
    prefix: {
      type: DataTypes.STRING(10),
      allowNull: true,
      comment: 'Optional prefix for labels'
    },
    columns: {
      type: DataTypes.INTEGER,
      allowNull: false,
      defaultValue: 1
    },
    rows: {
      type: DataTypes.INTEGER,
      allowNull: false,
      defaultValue: 1
    },
    start_index: {
      type: DataTypes.INTEGER,
      allowNull: false,
      comment: 'Starting sequential number for places in this rack'
    },
    sort_order: {
      type: DataTypes.INTEGER,
      defaultValue: 0
    },
    posX: {
      type: DataTypes.INTEGER,
      allowNull: true,
      comment: 'X position on blueprint canvas'
    },
    posY: {
      type: DataTypes.INTEGER,
      allowNull: true,
      comment: 'Y position on blueprint canvas'
    },
    warehouse_id: {
      type: DataTypes.INTEGER,
      allowNull: true,
      comment: 'Foreign key to warehouse'
    },
    visual_width: {
      type: DataTypes.INTEGER,
      allowNull: true,
      comment: 'Visual width on canvas'
    },
    visual_height: {
      type: DataTypes.INTEGER,
      allowNull: true,
      comment: 'Visual height on canvas'
    },
    rotation: {
      type: DataTypes.INTEGER,
      allowNull: true,
      defaultValue: 0,
      comment: 'Rotation angle in degrees'
    }
  }, {
    tableName: 'warehouse_racks',
    timestamps: true,
    indexes: [
      { fields: ['sort_order'] },
      { fields: ['warehouse_id'] }
    ]
  });
  return WarehouseRack;
};
