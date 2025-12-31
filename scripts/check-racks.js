require('dotenv').config();
const db = require('../src/shared/models/postgresql');

(async () => {
    await db.sequelize.authenticate();
    const [racks] = await db.sequelize.query(
        "SELECT id, name, warehouse_id FROM warehouse_racks ORDER BY id"
    );
    console.log('Actual warehouse_id values in DB:');
    racks.forEach(r => console.log(`  Rack #${r.id} (${r.name}): warehouse_id = ${r.warehouse_id}`));
    await db.sequelize.close();
    process.exit(0);
})();
