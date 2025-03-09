// services/dbService.js
const { Pool } = require('pg');
const logger = require('../utils/logging');

// Load database configuration from environment variables
const dbConfig = {
  host: process.env.DB_HOST || 'localhost',
  port: process.env.DB_PORT || 5432,
  database: process.env.DB_NAME || 'wms_db',
  user: process.env.DB_USER || 'wms_user',
  password: process.env.DB_PASSWORD || 'secure_password',
  max: 20, // Maximum number of clients in the pool
  idleTimeoutMillis: 30000, // Close idle clients after 30 seconds
  connectionTimeoutMillis: 2000 // Return an error after 2 seconds if connection could not be established
};

// Create a PostgreSQL connection pool
const pool = new Pool(dbConfig);

// Handle unexpected errors
pool.on('error', (err) => {
  logger.error(`Unexpected error on idle client: ${err.message}`);
  process.exit(-1);
});

/**
 * Database Service
 */
class DatabaseService {
  /**
   * Initialize the database service
   */
  constructor() {
    this.testConnection();
  }

  /**
   * Test the database connection
   * @returns {Promise<boolean>} True if connection successful
   */
  async testConnection() {
    const client = await pool.connect();
    try {
      await client.query('SELECT NOW()');
      logger.info('Database connection established successfully');
      return true;
    } catch (error) {
      logger.error(`Database connection failed: ${error.message}`);
      return false;
    } finally {
      client.release();
    }
  }

  /**
   * Execute a query with parameters
   * @param {string} text - SQL query text
   * @param {Array} params - Query parameters
   * @returns {Promise<Object>} Query result
   */
  async query(text, params = []) {
    const start = Date.now();
    try {
      const result = await pool.query(text, params);
      const duration = Date.now() - start;
      logger.debug(`Query executed in ${duration}ms: ${text}`);
      return result;
    } catch (error) {
      logger.error(`Query failed (${text}): ${error.message}`);
      throw error;
    }
  }

  /**
   * Begin a transaction
   * @returns {Object} Client for transaction
   */
  async beginTransaction() {
    const client = await pool.connect();
    try {
      await client.query('BEGIN');
      return client;
    } catch (error) {
      client.release();
      logger.error(`Failed to begin transaction: ${error.message}`);
      throw error;
    }
  }

  /**
   * Commit a transaction
   * @param {Object} client - Client used for transaction
   */
  async commitTransaction(client) {
    try {
      await client.query('COMMIT');
    } catch (error) {
      logger.error(`Failed to commit transaction: ${error.message}`);
      await this.rollbackTransaction(client);
      throw error;
    } finally {
      client.release();
    }
  }

  /**
   * Rollback a transaction
   * @param {Object} client - Client used for transaction
   */
  async rollbackTransaction(client) {
    try {
      await client.query('ROLLBACK');
    } catch (error) {
      logger.error(`Failed to rollback transaction: ${error.message}`);
    } finally {
      client.release();
    }
  }

  /**
   * Generate a new serial number
   * @param {string} type - Entity type ('i' for item, 'b' for box, 'p' for place)
   * @returns {Promise<string>} Generated serial number
   */
  async generateSerialNumber(type) {
    let counterName;
    switch (type) {
      case 'i':
        counterName = 'item_counter';
        break;
      case 'b':
        counterName = 'box_counter';
        break;
      case 'p':
        counterName = 'place_counter';
        break;
      default:
        throw new Error(`Unknown entity type: ${type}`);
    }

    const result = await this.query(
      'SELECT generate_serial_number($1, $2) AS serial_number',
      [counterName, type]
    );

    return result.rows[0].serial_number;
  }

  /**
   * Close the database connection pool
   */
  async close() {
    await pool.end();
    logger.info('Database connection pool closed');
  }
}

// Create and export a singleton instance
const dbService = new DatabaseService();
module.exports = dbService;