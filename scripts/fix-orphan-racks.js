require('dotenv').config();
const db = require('../src/shared/models/postgresql');

async function fix() {
  try {
    await db.sequelize.authenticate();
    console.log('Connected to DB...');

    // Get default warehouse ID
    const [defaultWarehouse] = await db.sequelize.query(
      'SELECT id FROM warehouses ORDER BY id ASC LIMIT 1',
      { type: db.sequelize.QueryTypes.SELECT }
    );

    if (!defaultWarehouse) {
      console.error('No warehouses found! Run migration 009 first.');
      process.exit(1);
    }

    console.log(`Default Warehouse ID: ${defaultWarehouse.id}`);

    // Update racks
    const [results, metadata] = await db.sequelize.query(
      'UPDATE warehouse_racks SET warehouse_id = :wid WHERE warehouse_id IS NULL',
      { replacements: { wid: defaultWarehouse.id } }
    );

    console.log(`✅ Fixed orphan racks. Affected rows: ${metadata.rowCount}`);
    process.exit(0);
  } catch (e) {
    console.error(e);
    process.exit(1);
  }
}

fix();
