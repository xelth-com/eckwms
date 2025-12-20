// scripts/run-migration.js
// Helper script to run SQL migrations on the PostgreSQL database
require('dotenv').config();
const { Client } = require('pg');
const fs = require('fs');
const path = require('path');

const client = new Client({
  user: process.env.PG_USERNAME,
  host: process.env.PG_HOST,
  database: process.env.PG_DATABASE,
  password: process.env.PG_PASSWORD,
  port: process.env.PG_PORT,
});

async function runMigration(migrationFile) {
  try {
    await client.connect();
    console.log('✓ Connected to database');

    const migrationPath = path.join(__dirname, '..', 'migrations', migrationFile);

    if (!fs.existsSync(migrationPath)) {
      throw new Error(`Migration file not found: ${migrationPath}`);
    }

    const sqlContent = fs.readFileSync(migrationPath, 'utf8');
    console.log(`\n📄 Running migration: ${migrationFile}`);
    console.log('─'.repeat(60));

    // Execute the SQL migration
    await client.query(sqlContent);

    console.log('─'.repeat(60));
    console.log('✓ Migration completed successfully!\n');

    // Verify the changes
    console.log('Verifying registered_devices table structure:');
    const columnsResult = await client.query(`
      SELECT column_name, data_type, column_default, is_nullable
      FROM information_schema.columns
      WHERE table_name = 'registered_devices'
      ORDER BY ordinal_position;
    `);

    console.log('\nColumns:');
    columnsResult.rows.forEach(col => {
      console.log(`  - ${col.column_name}: ${col.data_type}${col.column_default ? ` (default: ${col.column_default})` : ''}`);
    });

    // Check if status column was added
    const statusColumn = columnsResult.rows.find(col => col.column_name === 'status');
    if (statusColumn) {
      console.log('\n✓ Status column successfully added to registered_devices table');
    } else {
      console.warn('\n⚠ Warning: Status column not found in registered_devices table');
    }

  } catch (error) {
    console.error('\n✗ Migration failed:', error.message);
    console.error('\nFull error:', error);
    process.exit(1);
  } finally {
    await client.end();
    console.log('\n✓ Database connection closed');
  }
}

// Get migration file from command line argument or use default
const migrationFile = process.argv[2] || '003-add-device-status.sql';

console.log('\n' + '='.repeat(60));
console.log('  ECKWMS Database Migration Tool');
console.log('='.repeat(60));

runMigration(migrationFile);
