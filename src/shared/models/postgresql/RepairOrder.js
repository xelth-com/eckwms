// models/postgresql/RepairOrder.js
// InBody-specific model for repair orders
module.exports = (sequelize, DataTypes) => {
  const RepairOrder = sequelize.define('RepairOrder', {
    id: {
      type: DataTypes.INTEGER,
      primaryKey: true,
      autoIncrement: true
    },
    order_number: {
      type: DataTypes.STRING(50),
      unique: true,
      allowNull: false,
      comment: 'CS-DE-YYMMDD-XXX format'
    },
    year: {
      type: DataTypes.INTEGER,
      allowNull: false
    },
    date_of_receipt: {
      type: DataTypes.DATEONLY,
      allowNull: true
    },
    ticket_number: {
      type: DataTypes.STRING(50),
      allowNull: true
    },
    sequence: {
      type: DataTypes.INTEGER,
      allowNull: true
    },
    // Customer Info
    customer_name: {
      type: DataTypes.TEXT,
      allowNull: true
    },
    customer_email: {
      type: DataTypes.TEXT,
      allowNull: true
    },
    location: {
      type: DataTypes.TEXT,
      allowNull: true
    },
    // Device Info
    device_model: {
      type: DataTypes.STRING(50),
      allowNull: true
    },
    device_serial: {
      type: DataTypes.STRING(50),
      allowNull: true
    },
    // Repair Status
    warranty: {
      type: DataTypes.BOOLEAN,
      defaultValue: false
    },
    self_repair: {
      type: DataTypes.BOOLEAN,
      defaultValue: false
    },
    repair_status: {
      type: DataTypes.STRING(20),
      defaultValue: 'pending',
      validate: {
        isIn: [['pending', 'in_progress', 'completed', 'cancelled']]
      }
    },
    // Problem & Solution
    error_description: {
      type: DataTypes.TEXT,
      allowNull: true
    },
    error_category: {
      type: DataTypes.STRING(100),
      allowNull: true
    },
    troubleshooting: {
      type: DataTypes.TEXT,
      allowNull: true
    },
    solution_category: {
      type: DataTypes.STRING(100),
      allowNull: true
    },
    // File System Reference
    folder_path: {
      type: DataTypes.TEXT,
      allowNull: false
    },
    // Link to WMS scan
    scan_id: {
      type: DataTypes.UUID,
      allowNull: true,
      references: {
        model: 'scans',
        key: 'id'
      },
      comment: 'Link to scan that created or is associated with this repair order'
    },
    // Timestamps
    created_at: {
      type: DataTypes.DATE,
      defaultValue: DataTypes.NOW
    },
    updated_at: {
      type: DataTypes.DATE,
      defaultValue: DataTypes.NOW
    },
    completed_at: {
      type: DataTypes.DATE,
      allowNull: true
    },
    // Vector embeddings (for AI support)
    error_embedding: {
      type: 'vector(1536)',
      allowNull: true
    },
    solution_embedding: {
      type: 'vector(1536)',
      allowNull: true
    },
    // Full JSON backup (for Excel export)
    excel_data: {
      type: DataTypes.JSONB,
      allowNull: true
    }
  }, {
    tableName: 'repair_orders',
    timestamps: false, // We're managing timestamps manually
    underscored: true,
    indexes: [
      { fields: ['order_number'] },
      { fields: ['year'] },
      { fields: ['customer_name'] },
      { fields: ['device_model', 'device_serial'] },
      { fields: ['warranty'] },
      { fields: ['repair_status'] },
      { fields: ['scan_id'] }
    ]
  });

  return RepairOrder;
};
