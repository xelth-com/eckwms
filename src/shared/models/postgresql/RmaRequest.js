// models/postgresql/RmaRequest.js
module.exports = (sequelize, DataTypes) => {
  const RmaRequest = sequelize.define('RmaRequest', {
    id: {
      type: DataTypes.UUID,
      defaultValue: DataTypes.UUIDV4,
      primaryKey: true
    },
    userId: {
      type: DataTypes.UUID,
      allowNull: true, // Allow null for initial creation, link later
      references: {
        model: 'user_auth',
        key: 'id'
      }
    },
    rmaCode: {
      type: DataTypes.STRING,
      allowNull: false,
      unique: true
    },
    orderCode: {
      type: DataTypes.STRING,
      allowNull: false
    },
    status: {
      type: DataTypes.STRING,
      defaultValue: 'created',
      allowNull: false
    },
    company: {
      type: DataTypes.STRING,
      allowNull: false
    },
    person: {
      type: DataTypes.STRING,
      allowNull: true
    },
    street: {
      type: DataTypes.STRING,
      allowNull: false
    },
    houseNumber: {
      type: DataTypes.STRING,
      allowNull: true
    },
    postalCode: {
      type: DataTypes.STRING,
      allowNull: false
    },
    city: {
      type: DataTypes.STRING,
      allowNull: false
    },
    country: {
      type: DataTypes.STRING,
      allowNull: false
    },
    email: {
      type: DataTypes.STRING,
      allowNull: false
    },
    invoiceEmail: {
      type: DataTypes.STRING,
      allowNull: true
    },
    phone: {
      type: DataTypes.STRING,
      allowNull: true
    },
    resellerName: {
      type: DataTypes.STRING,
      allowNull: true
    },
    // Serialized device data
    devices: {
      type: DataTypes.JSONB,
      allowNull: false,
      defaultValue: []
    },
    // Original order data for legacy compatibility
    orderData: {
      type: DataTypes.JSONB,
      allowNull: true
    },
    receivedAt: {
      type: DataTypes.DATE,
      allowNull: true
    },
    processedAt: {
      type: DataTypes.DATE,
      allowNull: true
    },
    shippedAt: {
      type: DataTypes.DATE,
      allowNull: true
    },
    trackingNumber: {
      type: DataTypes.STRING,
      allowNull: true
    }
  }, {
    timestamps: true,
    tableName: 'rma_requests'
  });

  return RmaRequest;
};