require('dotenv').config();
const db = require('../src/shared/models/postgresql');

(async () => {
    await db.sequelize.authenticate();
    const [columns] = await db.sequelize.query(
        "SELECT column_name, data_type FROM information_schema.columns WHERE table_name = 'warehouse_racks' ORDER BY ordinal_position"
    );
    console.log('warehouse_racks table columns:');
    columns.forEach(col => console.log(`  - ${col.column_name} (${col.data_type})`));
    await db.sequelize.close();
    process.exit(0);
})();
