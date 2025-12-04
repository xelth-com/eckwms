/**
 * eckWMS Global Server - Sequelize Database Models
 *
 * Localized models for the standalone microservice
 * Imported from main app: models/postgresql/
 *
 * NOTE: This is a localized copy to ensure independence from parent directories
 * If data structure changes in main app, this needs to be updated manually
 * or synced via API calls to main app
 */

const { Sequelize, DataTypes } = require('sequelize');

// Initialize Sequelize with PostgreSQL
const sequelize = new Sequelize(
  process.env.PG_DATABASE || 'eckwms_global',
  process.env.PG_USERNAME || 'eckwms_user',
  process.env.PG_PASSWORD || 'secure_password',
  {
    host: process.env.PG_HOST || 'localhost',
    port: process.env.PG_PORT || 5432,
    dialect: 'postgres',
    logging: process.env.DB_LOGGING === 'true' ? console.log : false,
    dialectOptions: {
      application_name: 'eckwms-global-server'
    }
  }
);

const db = {};
db.Sequelize = Sequelize;
db.sequelize = sequelize;

/**
 * EckwmsInstance Model
 * Stores information about registered eckWMS instances
 */
db.EckwmsInstance = sequelize.define(
  'EckwmsInstance',
  {
    id: {
      type: DataTypes.UUID,
      defaultValue: DataTypes.UUIDV4,
      primaryKey: true,
      comment: 'Unique instance identifier'
    },
    name: {
      type: DataTypes.STRING,
      allowNull: false,
      unique: true,
      comment: 'Human-readable instance name'
    },
    server_url: {
      type: DataTypes.STRING,
      allowNull: false,
      comment: 'Primary server URL'
    },
    api_key: {
      type: DataTypes.STRING,
      allowNull: false,
      unique: true,
      comment: 'API key for authentication'
    },
    tier: {
      type: DataTypes.ENUM('free', 'paid'),
      defaultValue: 'free',
      allowNull: false,
      comment: 'Service tier (free/paid)'
    },
    publicIp: {
      type: DataTypes.STRING,
      allowNull: true,
      comment: 'Public IP address of the instance'
    },
    localIps: {
      type: DataTypes.JSONB,
      allowNull: true,
      defaultValue: [],
      comment: 'Array of local IP addresses'
    },
    tracerouteToGlobal: {
      type: DataTypes.TEXT,
      allowNull: true,
      comment: 'Traceroute diagnostic data'
    },
    serverPublicKey: {
      type: DataTypes.TEXT,
      allowNull: true,
      comment: 'Public key for secure communication'
    },
    lastSeen: {
      type: DataTypes.DATE,
      defaultValue: DataTypes.NOW,
      comment: 'Last heartbeat timestamp'
    }
  },
  {
    timestamps: true,
    tableName: 'eckwms_instances',
    underscored: true,
    comment: 'Stores registered eckWMS instances'
  }
);

/**
 * RegisteredDevice Model
 * Tracks devices registered with each instance
 */
db.RegisteredDevice = sequelize.define(
  'RegisteredDevice',
  {
    deviceId: {
      type: DataTypes.STRING,
      primaryKey: true,
      comment: 'Unique device identifier'
    },
    instance_id: {
      type: DataTypes.UUID,
      allowNull: false,
      comment: 'Reference to EckwmsInstance'
    },
    is_active: {
      type: DataTypes.BOOLEAN,
      defaultValue: true,
      comment: 'Whether device is actively connected'
    },
    deviceName: {
      type: DataTypes.STRING,
      allowNull: true,
      comment: 'Human-readable device name'
    },
    lastHeartbeat: {
      type: DataTypes.DATE,
      defaultValue: DataTypes.NOW,
      comment: 'Last device heartbeat'
    }
  },
  {
    timestamps: true,
    tableName: 'registered_devices',
    underscored: true,
    comment: 'Tracks devices registered with instances'
  }
);

/**
 * Scan Model
 * Stores QR code scan records
 */
db.Scan = sequelize.define(
  'Scan',
  {
    id: {
      type: DataTypes.UUID,
      defaultValue: DataTypes.UUIDV4,
      primaryKey: true
    },
    code: {
      type: DataTypes.STRING,
      allowNull: false,
      index: true,
      comment: 'QR code value'
    },
    instance_id: {
      type: DataTypes.UUID,
      allowNull: true,
      comment: 'Instance that performed the scan'
    },
    device_id: {
      type: DataTypes.STRING,
      allowNull: true,
      comment: 'Device that performed the scan'
    },
    timestamp: {
      type: DataTypes.DATE,
      defaultValue: DataTypes.NOW,
      comment: 'When the scan occurred'
    }
  },
  {
    timestamps: true,
    tableName: 'scans',
    underscored: true,
    comment: 'Records of QR code scans'
  }
);

// Define associations
db.EckwmsInstance.hasMany(db.RegisteredDevice, {
  foreignKey: 'instance_id',
  onDelete: 'CASCADE'
});

db.RegisteredDevice.belongsTo(db.EckwmsInstance, {
  foreignKey: 'instance_id'
});

module.exports = db;
