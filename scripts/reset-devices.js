require('dotenv').config();
const db = require('../src/shared/models/postgresql');

async function resetDevices() {
  try {
    console.log('🗑️  Connecting to database...');
    await db.sequelize.authenticate();

    console.log('🧹 Clearing registered_devices table...');
    // Using destroy with truncate option is faster and resets auto-increments if any
    await db.RegisteredDevice.destroy({
      where: {},
      truncate: true,
      cascade: true // If there are related records
    });

    console.log('✅ All devices have been removed.');
    console.log('   Next device connection will be treated as a fresh registration (Pending).');

    await db.sequelize.close();
    process.exit(0);
  } catch (error) {
    console.error('❌ Error clearing devices:', error.message);
    process.exit(1);
  }
}

resetDevices();
