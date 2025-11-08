// models/postgresql/index.js
const { Sequelize } = require('sequelize');

// Initialize Sequelize with DB connection parameters
const sequelize = new Sequelize(
  process.env.PG_DATABASE,
  process.env.PG_USERNAME,
  process.env.PG_PASSWORD,
  {
    host: process.env.PG_HOST,
    port: process.env.PG_PORT,
    dialect: 'postgres',
    logging: process.env.NODE_ENV !== 'production',
    pool: {
      max: 10,
      min: 0,
      acquire: 30000,
      idle: 10000
    }
  }
);

const db = {};

db.Sequelize = Sequelize;
db.sequelize = sequelize;

// Import models
db.UserAuth = require('./UserAuth')(sequelize, Sequelize);
db.RmaRequest = require('./RmaRequest')(sequelize, Sequelize);
db.TranslationCache = require('./TranslationCache')(sequelize, Sequelize);
db.Scan = require('./Scan')(sequelize, Sequelize);
db.EckwmsInstance = require('./EckwmsInstance')(sequelize, Sequelize);
db.RegisteredDevice = require('./RegisteredDevice')(sequelize, Sequelize);
db.PublicData = require('./PublicData')(sequelize, Sequelize);

// Define relationships
db.UserAuth.hasMany(db.RmaRequest, { foreignKey: 'userId' });
db.RmaRequest.belongsTo(db.UserAuth, { foreignKey: 'userId' });

// EckWMS relationships
db.EckwmsInstance.hasMany(db.Scan, { foreignKey: 'instance_id' });
db.Scan.belongsTo(db.EckwmsInstance, { foreignKey: 'instance_id' });

db.EckwmsInstance.hasMany(db.RegisteredDevice, { foreignKey: 'instance_id' });
db.RegisteredDevice.belongsTo(db.EckwmsInstance, { foreignKey: 'instance_id' });

module.exports = db;