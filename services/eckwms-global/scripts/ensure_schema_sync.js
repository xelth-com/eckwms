require('dotenv').config({ path: './services/eckwms-global/.env' });
const { Client } = require('pg');

async function run() {
  const client = new Client({
    host: process.env.PG_HOST || 'localhost',
    port: process.env.PG_PORT || 5432,
    database: process.env.PG_DATABASE || 'eckwms_global',
    user: process.env.PG_USERNAME,
    password: process.env.PG_PASSWORD
  });

  try {
    await client.connect();
    console.log('Connected to global DB.');

    // 1. Check/Add status column to registered_devices
    const checkCol = await client.query(
      "SELECT column_name FROM information_schema.columns WHERE table_name='registered_devices' AND column_name='status'"
    );

    if (checkCol.rows.length === 0) {
      console.log('Adding missing column: status...');
      // Create ENUM if not exists (handled gracefully by Postgres usually, but we use text/check for safety in microservice or just raw enum)
      try {
        await client.query("CREATE TYPE device_status AS ENUM ('active', 'pending', 'blocked');");
      } catch (e) { /* ignore if type exists */ }

      await client.query('ALTER TABLE registered_devices ADD COLUMN status VARCHAR(20) DEFAULT \'pending\';');
      console.log('Column added.');
    } else {
      console.log('Column status already exists.');
    }

    console.log('Schema sync complete.');
  } catch (e) {
    console.error('Migration failed:', e);
    process.exit(1);
  } finally {
    await client.end();
  }
}

run();
