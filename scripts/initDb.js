const pgp = require('pg-promise')();
const db = pgp('postgres://wms_user:secure_password@localhost:5432/wms_db');

async function initDb() {
  try {
    await db.none(`
      CREATE TABLE IF NOT EXISTS users (...);
      CREATE TABLE IF NOT EXISTS items (...);
      CREATE TABLE IF NOT EXISTS boxes (...);
      CREATE TABLE IF NOT EXISTS orders (...);
      CREATE TABLE IF NOT EXISTS item_history (...);
      CREATE TABLE IF NOT EXISTS box_history (...);
    `);
    console.log('Database initialized successfully');
  } catch (error) {
    console.error('Failed to initialize database:', error);
  } finally {
    pgp.end();
  }
}

initDb();