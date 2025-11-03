// models/postgresql/Scan.js
module.exports = (sequelize, DataTypes) => {
  const Scan = sequelize.define('Scan', {
    id: {
      type: DataTypes.UUID,
      defaultValue: DataTypes.UUIDV4,
      primaryKey: true
    },
    payload: {
      type: DataTypes.TEXT,
      allowNull: true,
      defaultValue: '{}',
      comment: 'Scan payload data (migrated from barcode field)'
    },
    checksum: {
      type: DataTypes.STRING,
      allowNull: true,
      defaultValue: '',
      comment: 'SHA256 checksum for atomicity verification'
    },
    instance_id: {
      type: DataTypes.UUID,
      allowNull: true,
      references: {
        model: 'eckwms_instances',
        key: 'id'
      },
      comment: 'Reference to the client eckWMS instance'
    },
    deviceId: {
      type: DataTypes.STRING,
      allowNull: true
    },
    type: {
      type: DataTypes.STRING,
      allowNull: true,
      comment: 'Scan type/category'
    },
    priority: {
      type: DataTypes.INTEGER,
      defaultValue: 0,
      comment: 'Priority level for retention logic'
    },
    status: {
      type: DataTypes.STRING,
      defaultValue: 'buffered',
      allowNull: false,
      comment: 'Scan status: buffered, delivered, or confirmed'
    }
  }, {
    timestamps: true,
    tableName: 'scans',
    indexes: [
      {
        fields: ['instance_id', 'status']
      },
      {
        fields: ['instance_id', 'createdAt']
      }
    ]
  });

  return Scan;
};
