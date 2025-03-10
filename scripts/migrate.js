// scripts/migrate.js
require('dotenv').config();
const fs = require('fs').promises;
const path = require('path');
const readline = require('readline');
const { createReadStream, createWriteStream } = require('fs');
const { Client } = require('pg');
const { createHash } = require('crypto');
const { v4: uuidv4 } = require('uuid');


const pgp = require('pg-promise')();
const fs = require('fs');
const readline = require('readline');

// Конфигурация базы данных
const db = pgp({
  host: 'localhost',
  port: 5432,
  database: 'wms_db',
  user: 'wms_user',
  password: 'secure_password'
});

// Функция для параллельной обработки коллекций
async function migrateCollections() {
  const collections = [
    { name: 'items', file: 'items.json' },
    { name: 'boxes', file: 'boxes.json' },
    { name: 'places', file: 'places.json' }
  ];

  await Promise.all(collections.map(async (collection) => {
    const fileStream = fs.createReadStream(`./data/${collection.file}`);
    const rl = readline.createInterface({
      input: fileStream,
      crlfDelay: Infinity
    });

    const data = [];

    for await (const line of rl) {
      data.push(JSON.parse(line));
    }

    await db.none(pgp.helpers.insert(data, collection.name));
    console.log(`Migrated ${data.length} records into ${collection.name}`);
  }));
}

// Функция для миграции пользовательских данных
async function migrateUsers() {
  const fileStream = fs.createReadStream('./data/users.json');
  const rl = readline.createInterface({
    input: fileStream,
    crlfDelay: Infinity
  });

  const users = [];

  for await (const line of rl) {
    users.push(JSON.parse(line));
  }

  await db.none(pgp.helpers.insert(users, 'users'));
  console.log(`Migrated ${users.length} users`);
}

// Основная функция миграции
async function migrate() {
  try {
    await migrateCollections();
    await migrateUsers();
    console.log('Migration completed successfully');
  } catch (error) {
    console.error('Migration failed:', error);
  } finally {
    pgp.end();
  }
}




// Configuration
const CONFIG = {
  sourceDir: process.env.SOURCE_DIR || './data/legacy',
  tempDir: process.env.TEMP_DIR || './data/temp',
  logDir: process.env.LOG_DIR || './logs',
  batchSize: parseInt(process.env.BATCH_SIZE || '100'),
  validateOnly: process.env.VALIDATE_ONLY === 'true',
  dbConfig: {
    host: process.env.DB_HOST || 'localhost',
    port: parseInt(process.env.DB_PORT || '5432'),
    database: process.env.DB_NAME || 'wms_db',
    user: process.env.DB_USER || 'wms_user',
    password: process.env.DB_PASSWORD
  }
};

// Create logger
const logger = {
  info: (message) => {
    const timestamp = new Date().toISOString();
    console.log(`[${timestamp}] INFO: ${message}`);
    fs.appendFile(
      path.join(CONFIG.logDir, 'migration.log'),
      `[${timestamp}] INFO: ${message}\n`
    ).catch(err => console.error('Error writing to log:', err));
  },
  error: (message, error) => {
    const timestamp = new Date().toISOString();
    console.error(`[${timestamp}] ERROR: ${message}`, error);
    fs.appendFile(
      path.join(CONFIG.logDir, 'migration.log'),
      `[${timestamp}] ERROR: ${message} ${error ? error.stack || error : ''}\n`
    ).catch(err => console.error('Error writing to log:', err));
  },
  warn: (message) => {
    const timestamp = new Date().toISOString();
    console.warn(`[${timestamp}] WARN: ${message}`);
    fs.appendFile(
      path.join(CONFIG.logDir, 'migration.log'),
      `[${timestamp}] WARN: ${message}\n`
    ).catch(err => console.error('Error writing to log:', err));
  }
};

// Statistics tracking
const stats = {
  started: new Date(),
  processed: {
    users: 0,
    items: 0,
    boxes: 0,
    places: 0,
    orders: 0,
    classes: 0,
    uppers: 0,
    dicts: 0
  },
  errors: {
    users: 0,
    items: 0,
    boxes: 0,
    places: 0,
    orders: 0,
    classes: 0,
    uppers: 0,
    dicts: 0
  },
  skipped: {
    users: 0,
    items: 0,
    boxes: 0,
    places: 0,
    orders: 0,
    classes: 0,
    uppers: 0,
    dicts: 0
  },
  duplicates: {
    users: 0,
    items: 0,
    boxes: 0,
    places: 0,
    orders: 0,
    classes: 0,
    uppers: 0,
    dicts: 0
  }
};

// Cached lookup maps (to avoid repeated database queries)
const cache = {
  users: new Map(),
  classes: new Map(),
  places: new Map(),
  items: new Map(),
  boxes: new Map()
};

/**
 * Initialize the migration environment
 */
async function initialize() {
  logger.info('Starting migration initialization');
  
  try {
    // Create directories if they don't exist
    await Promise.all([
      fs.mkdir(CONFIG.tempDir, { recursive: true }),
      fs.mkdir(CONFIG.logDir, { recursive: true })
    ]);
    
    // Validate source directory
    await fs.access(CONFIG.sourceDir);
    
    logger.info('Migration environment initialized successfully');
    return true;
  } catch (error) {
    logger.error('Failed to initialize migration environment', error);
    return false;
  }
}

/**
 * Connect to the database
 */
async function connectToDatabase() {
  logger.info('Connecting to database');
  
  const client = new Client(CONFIG.dbConfig);
  
  try {
    await client.connect();
    logger.info('Database connection established');
    return client;
  } catch (error) {
    logger.error('Failed to connect to database', error);
    throw error;
  }
}

/**
 * Read data from legacy system files
 * @param {string} collection - Collection name
 * @returns {Promise<Array>} - Parsed data objects
 */
async function readLegacyData(collection) {
  logger.info(`Reading legacy data for collection: ${collection}`);
  
  const filePath = path.join(CONFIG.sourceDir, `base/${collection}.json`);
  const items = [];
  
  try {
    await fs.access(filePath);
    
    const rl = readline.createInterface({
      input: createReadStream(filePath),
      crlfDelay: Infinity
    });
    
    for await (const line of rl) {
      try {
        const item = JSON.parse(line);
        items.push(item);
      } catch (error) {
        logger.error(`Failed to parse line in ${collection}`, error);
        stats.errors[collection]++;
      }
    }
    
    logger.info(`Read ${items.length} items from ${collection}`);
    return items;
  } catch (error) {
    logger.error(`Failed to read ${collection} data`, error);
    throw error;
  }
}

/**
 * Extract the reference ID from a legacy serial number
 * @param {string} serialNumber - Legacy serial number
 * @returns {string} - UUID
 */
function getReferenceId(serialNumber) {
  if (!serialNumber) return null;
  
  // Check if we already have a cached UUID for this serial number
  if (cache[serialNumber]) {
    return cache[serialNumber];
  }
  
  // Generate a deterministic UUID based on the serial number
  const hash = createHash('md5').update(serialNumber).digest('hex');
  const uuid = uuidv4({ random: Buffer.from(hash, 'hex').slice(0, 16) });
  
  // Cache the result
  cache[serialNumber] = uuid;
  
  return uuid;
}

/**
 * Clean and transform user data
 * @param {Object} user - Legacy user object
 * @returns {Object} - Transformed user data
 */
function transformUser(user) {
  if (!user || !user.sn || !user.sn[0]) {
    throw new Error('Invalid user data: missing serial number');
  }
  
  const id = getReferenceId(user.sn[0]);
  const username = user.sn[0].replace(/^u/, '').slice(0, 50);
  
  // Generate a temporary password hash (would be reset by user)
  const tempPasswordHash = createHash('sha256').update(`temp_${username}_${Date.now()}`).digest('hex');
  
  return {
    id,
    username,
    password_hash: tempPasswordHash,
    company: user.comp || null,
    email: user.cem || null,
    phone: user.ph || null,
    street: user.str || null,
    house_number: user.hs || null,
    postal_code: user.zip || null,
    city: user.cit || null,
    country: user.ctry || null,
    role: 'user', // Default role
    created_at: new Date(),
    updated_at: new Date()
  };
}

/**
 * Clean and transform class data
 * @param {Object} cls - Legacy class object
 * @returns {Object} - Transformed class data
 */
function transformClass(cls) {
  if (!cls || !cls.cl) {
    throw new Error('Invalid class data: missing class name');
  }
  
  const id = getReferenceId(cls.cl);
  
  // Format properties
  const properties = {};
  if (cls.prop && Array.isArray(cls.prop)) {
    cls.prop.forEach(prop => {
      if (Array.isArray(prop) && prop.length > 0) {
        properties[prop[0]] = prop.slice(1);
      }
    });
  }
  
  // Format relations
  const relations = {};
  if (cls.rel && Array.isArray(cls.rel)) {
    cls.rel.forEach(rel => {
      if (Array.isArray(rel) && rel.length > 1) {
        relations[rel[0]] = rel.slice(1);
      }
    });
  }
  
  // Format part numbers
  const partNumbers = [];
  if (cls.pn && Array.isArray(cls.pn)) {
    cls.pn.forEach(pn => {
      if (Array.isArray(pn) && pn.length >= 2) {
        partNumbers.push({
          number: pn[0],
          type: pn[1]
        });
      }
    });
  }
  
  return {
    id,
    class_name: cls.cl,
    part_numbers: JSON.stringify(partNumbers),
    description: JSON.stringify(cls.desc || []),
    properties: JSON.stringify(properties),
    relations: JSON.stringify(relations),
    created_at: new Date(),
    updated_at: new Date()
  };
}

/**
 * Clean and transform item data
 * @param {Object} item - Legacy item object
 * @returns {Object} - Transformed item data
 */
function transformItem(item) {
  if (!item || !item.sn || !item.sn[0]) {
    throw new Error('Invalid item data: missing serial number');
  }
  
  const id = getReferenceId(item.sn[0]);
  const serialNumber = item.sn[0];
  
  // Get creation timestamp
  let createdAt;
  if (item.sn && item.sn.length > 1 && item.sn[1]) {
    createdAt = new Date(item.sn[1] * 1000); // Convert Unix timestamp to Date
  } else {
    createdAt = new Date();
  }
  
  // Get class ID
  let classId = null;
  if (item.cl) {
    classId = getReferenceId(item.cl);
    cache.classes.set(item.cl, classId);
  }
  
  // Format actions
  const actions = [];
  if (item.actn && Array.isArray(item.actn)) {
    item.actn.forEach(action => {
      if (Array.isArray(action) && action.length >= 3) {
        actions.push({
          type: action[0],
          message: action[1],
          timestamp: action[2]
        });
      }
    });
  }
  
  // Get current location and history
  let currentLocationId = null;
  const locationHistory = [];
  
  if (item.loc && Array.isArray(item.loc)) {
    item.loc.forEach(loc => {
      if (Array.isArray(loc) && loc.length >= 2) {
        const locationId = loc[0];
        const timestamp = loc[1];
        
        locationHistory.push({
          id: locationId,
          timestamp: timestamp
        });
        
        // Last location becomes current location
        currentLocationId = locationId;
      }
    });
  }
  
  return {
    id,
    serial_number: serialNumber,
    class_id: classId,
    created_at: createdAt,
    description: JSON.stringify(item.desc || []),
    condition: JSON.stringify(item.cond || []),
    actions: JSON.stringify(actions),
    images: JSON.stringify(item.img || []),
    mass: JSON.stringify(item.mas || []),
    size: JSON.stringify(item.siz || []),
    owner: JSON.stringify(item.own || []),
    barcodes: JSON.stringify(item.brc || []),
    location_history: JSON.stringify(locationHistory),
    current_location_id: currentLocationId,
    attributes: JSON.stringify(item.attr || {})
  };
}

/**
 * Clean and transform box data
 * @param {Object} box - Legacy box object
 * @returns {Object} - Transformed box data
 */
function transformBox(box) {
  if (!box || !box.sn || !box.sn[0]) {
    throw new Error('Invalid box data: missing serial number');
  }
  
  const id = getReferenceId(box.sn[0]);
  const serialNumber = box.sn[0];
  
  // Get creation timestamp
  let createdAt;
  if (box.sn && box.sn.length > 1 && box.sn[1]) {
    createdAt = new Date(box.sn[1] * 1000); // Convert Unix timestamp to Date
  } else {
    createdAt = new Date();
  }
  
  // Get class ID
  let classId = null;
  if (box.cl) {
    classId = getReferenceId(box.cl);
  }
  
  // Format contents
  const contents = [];
  if (box.cont && Array.isArray(box.cont)) {
    box.cont.forEach(cont => {
      if (Array.isArray(cont) && cont.length >= 2) {
        const itemId = cont[0];
        const timestamp = cont[1];
        
        contents.push({
          id: getReferenceId(itemId),
          timestamp: timestamp
        });
      }
    });
  }
  
  // Format incoming
  const incoming = [];
  if (box.in && Array.isArray(box.in)) {
    box.in.forEach(inItem => {
      if (Array.isArray(inItem) && inItem.length >= 2) {
        const sourceId = inItem[0];
        const timestamp = inItem[1];
        
        incoming.push({
          id: getReferenceId(sourceId),
          timestamp: timestamp
        });
      }
    });
  }
  
  // Format outgoing
  const outgoing = [];
  if (box.out && Array.isArray(box.out)) {
    box.out.forEach(outItem => {
      if (Array.isArray(outItem) && outItem.length >= 2) {
        const destinationId = outItem[0];
        const timestamp = outItem[1];
        
        outgoing.push({
          id: getReferenceId(destinationId),
          timestamp: timestamp
        });
      }
    });
  }
  
  // Get current location and history
  let currentLocationId = null;
  const locationHistory = [];
  
  if (box.loc && Array.isArray(box.loc)) {
    box.loc.forEach(loc => {
      if (Array.isArray(loc) && loc.length >= 2) {
        const locationId = loc[0];
        const timestamp = loc[1];
        
        locationHistory.push({
          id: locationId,
          timestamp: timestamp
        });
        
        // Last location becomes current location
        currentLocationId = locationId;
      }
    });
  }
  
  return {
    id,
    serial_number: serialNumber,
    class_id: classId,
    created_at: createdAt,
    description: JSON.stringify(box.desc || []),
    barcodes: JSON.stringify(box.brc || []),
    mass: JSON.stringify(box.mas || []),
    size: JSON.stringify(box.siz || []),
    contents: JSON.stringify(contents),
    incoming: JSON.stringify(incoming),
    outgoing: JSON.stringify(outgoing),
    multiplier: box.mult && Array.isArray(box.mult) && box.mult.length > 0 ? 
      parseInt(box.mult[0][0] || 1) : 1,
    location_history: JSON.stringify(locationHistory),
    current_location_id: currentLocationId
  };
}

/**
 * Clean and transform place data
 * @param {Object} place - Legacy place object
 * @returns {Object} - Transformed place data
 */
function transformPlace(place) {
  if (!place || !place.sn || !place.sn[0]) {
    throw new Error('Invalid place data: missing serial number');
  }
  
  const id = getReferenceId(place.sn[0]);
  const serialNumber = place.sn[0];
  
  // Get creation timestamp
  let createdAt;
  if (place.sn && place.sn.length > 1 && place.sn[1]) {
    createdAt = new Date(place.sn[1] * 1000); // Convert Unix timestamp to Date
  } else {
    createdAt = new Date();
  }
  
  // Get class ID
  let classId = null;
  if (place.cl) {
    classId = getReferenceId(place.cl);
  }
  
  // Format contents
  const contents = [];
  if (place.cont && Array.isArray(place.cont)) {
    place.cont.forEach(cont => {
      if (Array.isArray(cont) && cont.length >= 2) {
        const itemId = cont[0];
        const timestamp = cont[1];
        
        contents.push({
          id: getReferenceId(itemId),
          timestamp: timestamp
        });
      }
    });
  }
  
  return {
    id,
    serial_number: serialNumber,
    class_id: classId,
    created_at: createdAt,
    description: JSON.stringify(place.desc || []),
    contents: JSON.stringify(contents)
  };
}

/**
 * Clean and transform order data
 * @param {Object} order - Legacy order object
 * @returns {Object} - Transformed order data
 */
function transformOrder(order) {
  if (!order || !order.sn || !order.sn[0]) {
    throw new Error('Invalid order data: missing serial number');
  }
  
  const id = getReferenceId(order.sn[0]);
  const serialNumber = order.sn[0];
  
  // Get creation timestamp
  let createdAt;
  if (order.sn && order.sn.length > 1 && order.sn[1]) {
    createdAt = new Date(order.sn[1] * 1000); // Convert Unix timestamp to Date
  } else {
    createdAt = new Date();
  }
  
  // Get class ID
  let classId = null;
  if (order.cl) {
    classId = getReferenceId(order.cl);
  }
  
  // Get customer ID
  let customerId = null;
  if (order.cust && Array.isArray(order.cust) && order.cust.length > 0) {
    customerId = getReferenceId(order.cust[0]);
  }
  
  // Format contents
  const contents = [];
  if (order.cont && Array.isArray(order.cont)) {
    order.cont.forEach(cont => {
      if (Array.isArray(cont) && cont.length >= 2) {
        const itemId = cont[0];
        const timestamp = cont[1];
        
        contents.push({
          id: getReferenceId(itemId),
          timestamp: timestamp
        });
      }
    });
  }
  
  // Format declarations
  const declarations = [];
  if (order.decl && Array.isArray(order.decl)) {
    order.decl.forEach(decl => {
      if (Array.isArray(decl) && decl.length >= 2) {
        declarations.push([decl[0], decl[1]]);
      }
    });
  }
  
  return {
    id,
    serial_number: serialNumber,
    class_id: classId,
    created_at: createdAt,
    customer_id: customerId,
    company: order.comp || null,
    person: order.pers || null,
    street: order.str || null,
    house_number: order.hs || null,
    postal_code: order.zip || null,
    city: order.cit || null,
    country: order.ctry || null,
    contact_email: order.cem || null,
    invoice_email: order.iem || null,
    phone: order.ph || null,
    contents: JSON.stringify(contents),
    declarations: JSON.stringify(declarations)
  };
}

/**
 * Clean and transform dictionary data
 * @param {Object} dict - Legacy dictionary object
 * @returns {Object} - Transformed dictionary data
 */
function transformDict(dict) {
  if (!dict || !dict.translations || !dict.translations.original) {
    throw new Error('Invalid dict data: missing original text');
  }
  
  const id = uuidv4();
  const original = dict.translations.original;
  
  // Extract translations
  const translations = [];
  for (const [langCode, translatedText] of Object.entries(dict.translations)) {
    if (langCode !== 'original') {
      translations.push({
        language_code: langCode,
        translated_text: translatedText
      });
    }
  }
  
  return {
    id,
    original_text: original,
    translations
  };
}

/**
 * Insert data into PostgreSQL database
 * @param {Object} client - Database client
 * @param {string} tableName - Target table name
 * @param {Array} data - Data to insert
 * @returns {Promise<number>} - Number of records inserted
 */
async function insertData(client, tableName, data) {
  if (!data || data.length === 0) {
    return 0;
  }
  
  logger.info(`Inserting ${data.length} records into ${tableName}`);
  
  // Start a transaction
  await client.query('BEGIN');
  
  try {
    let inserted = 0;
    
    // Process in batches
    for (let i = 0; i < data.length; i += CONFIG.batchSize) {
      const batch = data.slice(i, i + CONFIG.batchSize);
      
      // Generate query for batch insert
      if (batch.length > 0) {
        const columns = Object.keys(batch[0]);
        const valuePlaceholders = [];
        const values = [];
        
        batch.forEach((record, recordIndex) => {
          const rowPlaceholders = [];
          
          columns.forEach((column, columnIndex) => {
            const paramIndex = recordIndex * columns.length + columnIndex + 1;
            rowPlaceholders.push(`$${paramIndex}`);
            values.push(record[column]);
          });
          
          valuePlaceholders.push(`(${rowPlaceholders.join(', ')})`);
        });
        
        const query = `
          INSERT INTO ${tableName} (${columns.join(', ')})
          VALUES ${valuePlaceholders.join(', ')}
          ON CONFLICT DO NOTHING
        `;
        
        if (!CONFIG.validateOnly) {
          const result = await client.query(query, values);
          inserted += result.rowCount;
        } else {
          // In validation mode, just log the query
          inserted += batch.length;
        }
        
        logger.info(`Inserted batch of ${batch.length} records into ${tableName}`);
      }
    }
    
    // Commit transaction
    await client.query('COMMIT');
    
    logger.info(`Successfully inserted ${inserted} records into ${tableName}`);
    return inserted;
  } catch (error) {
    // Rollback on error
    await client.query('ROLLBACK');
    logger.error(`Failed to insert data into ${tableName}`, error);
    throw error;
  }
}

/**
 * Insert translations
 * @param {Object} client - Database client
 * @param {Array} dicts - Dictionary data
 * @returns {Promise<number>} - Number of translations inserted
 */
async function insertTranslations(client, dicts) {
  if (!dicts || dicts.length === 0) {
    return 0;
  }
  
  logger.info(`Processing ${dicts.length} dictionaries`);
  
  // Start a transaction
  await client.query('BEGIN');
  
  try {
    let inserted = 0;
    
    for (const dict of dicts) {
      // Insert original text
      const originalResult = await client.query(
        `INSERT INTO translations (id, original_text, language_code, translated_text)
         VALUES ($1, $2, 'original', $2)
         ON CONFLICT DO NOTHING
         RETURNING id`,
        [dict.id, dict.original_text]
      );
      
      if (originalResult.rowCount > 0) {
        inserted++;
      }
      
      // Insert translations
      for (const translation of dict.translations) {
        const translationResult = await client.query(
          `INSERT INTO translations (id, original_text, language_code, translated_text)
           VALUES ($1, $2, $3, $4)
           ON CONFLICT DO NOTHING`,
          [
            uuidv4(),
            dict.original_text,
            translation.language_code,
            translation.translated_text
          ]
        );
        
        if (translationResult.rowCount > 0) {
          inserted++;
        }
      }
    }
    
    // Commit transaction
    await client.query('COMMIT');
    
    logger.info(`Successfully inserted ${inserted} translations`);
    return inserted;
  } catch (error) {
    // Rollback on error
    await client.query('ROLLBACK');
    logger.error('Failed to insert translations', error);
    throw error;
  }
}

/**
 * Initialize serial counters
 * @param {Object} client - Database client
 * @returns {Promise<boolean>} - Success status
 */
async function initializeSerialCounters(client) {
  logger.info('Initializing serial counters');
  
  // Start a transaction
  await client.query('BEGIN');
  
  try {
    // Read the initial values from ini.json
    const iniPath = path.join(CONFIG.sourceDir, 'base/ini.json');
    const iniData = JSON.parse(await fs.readFile(iniPath, 'utf8'));
    
    // Insert or update serial counters
    const counters = [
      { name: 'item_counter', value: iniData.serialI || 1 },
      { name: 'box_counter', value: iniData.serialB || 1 },
      { name: 'place_counter', value: iniData.serialP || 1 },
      { name: 'order_counter', value: iniData.serialIi || 999999999999999 }
    ];
    
    for (const counter of counters) {
      await client.query(
        `INSERT INTO serial_counters (counter_name, current_value)
         VALUES ($1, $2)
         ON CONFLICT (counter_name) 
         DO UPDATE SET current_value = $2, updated_at = NOW()`,
        [counter.name, counter.value]
      );
    }
    
    // Commit transaction
    await client.query('COMMIT');
    
    logger.info('Serial counters initialized successfully');
    return true;
  } catch (error) {
    // Rollback on error
    await client.query('ROLLBACK');
    logger.error('Failed to initialize serial counters', error);
    return false;
  }
}

/**
 * Print migration statistics
 */
function printStats() {
  const duration = new Date() - stats.started;
  const minutes = Math.floor(duration / 60000);
  const seconds = ((duration % 60000) / 1000).toFixed(2);
  
  console.log('\n=== Migration Statistics ===');
  console.log(`Duration: ${minutes} minutes, ${seconds} seconds`);
  console.log('\nProcessed Records:');
  console.log(`- Users: ${stats.processed.users}`);
  console.log(`- Classes: ${stats.processed.classes}`);
  console.log(`- Items: ${stats.processed.items}`);
  console.log(`- Boxes: ${stats.processed.boxes}`);
  console.log(`- Places: ${stats.processed.places}`);
  console.log(`- Orders: ${stats.processed.orders}`);
  console.log(`- Dictionaries: ${stats.processed.dicts}`);
  
  console.log('\nErrors:');
  console.log(`- Users: ${stats.errors.users}`);
  console.log(`- Classes: ${stats.errors.classes}`);
  console.log(`- Items: ${stats.errors.items}`);
  console.log(`- Boxes: ${stats.errors.boxes}`);
  console.log(`- Places: ${stats.errors.places}`);
  console.log(`- Orders: ${stats.errors.orders}`);
  console.log(`- Dictionaries: ${stats.errors.dicts}`);
  
  console.log('\nSkipped Records:');
  console.log(`- Users: ${stats.skipped.users}`);
  console.log(`- Classes: ${stats.skipped.classes}`);
  console.log(`- Items: ${stats.skipped.items}`);
  console.log(`- Boxes: ${stats.skipped.boxes}`);
  console.log(`- Places: ${stats.skipped.places}`);
  console.log(`- Orders: ${stats.skipped.orders}`);
  console.log(`- Dictionaries: ${stats.skipped.dicts}`);
  
  console.log('\nDuplicates:');
  console.log(`- Users: ${stats.duplicates.users}`);
  console.log(`- Classes: ${stats.duplicates.classes}`);
  console.log(`- Items: ${stats.duplicates.items}`);
  console.log(`- Boxes: ${stats.duplicates.boxes}`);
  console.log(`- Places: ${stats.duplicates.places}`);
  console.log(`- Orders: ${stats.duplicates.orders}`);
  console.log(`- Dictionaries: ${stats.duplicates.dicts}`);
  
  console.log('\n=============================');
}

/**
 * Main migration function
 */
async function migrate() {
  logger.info('Starting data migration');
  logger.info(`Mode: ${CONFIG.validateOnly ? 'Validation Only' : 'Full Migration'}`);
  
  // Initialize environment
  const initialized = await initialize();
  if (!initialized) {
    logger.error('Migration aborted due to initialization failure');
    return;
  }
  
  // Connect to the database
  let client;
  try {
    client = await connectToDatabase();
  } catch (error) {
    logger.error('Migration aborted due to database connection failure');
    return;
  }
  
  try {
    // 1. Migrate classes first (needed for references)
    logger.info('Step 1: Migrating classes');
    const classesData = await readLegacyData('classes');
    const transformedClasses = [];
    
    for (const cls of classesData) {
      try {
        const transformedClass = transformClass(cls);
        transformedClasses.push(transformedClass);
        stats.processed.classes++;
        
        // Cache class ID for later reference
        cache.classes.set(cls.cl, transformedClass.id);
      } catch (error) {
        logger.error(`Failed to transform class: ${cls.cl}`, error);
        stats.errors.classes++;
      }
    }
    
    await insertData(client, 'classes', transformedClasses);
    
    // 2. Migrate places (needed for location references)
    logger.info('Step 2: Migrating places');
    const placesData = await readLegacyData('places');
    const transformedPlaces = [];
    
    for (const place of placesData) {
      try {
        const transformedPlace = transformPlace(place);
        transformedPlaces.push(transformedPlace);
        stats.processed.places++;
        
        // Cache place ID for later reference
        cache.places.set(place.sn[0], transformedPlace.id);
      } catch (error) {
        logger.error(`Failed to transform place: ${place.sn?.[0]}`, error);
        stats.errors.places++;
      }
    }
    
    await insertData(client, 'places', transformedPlaces);
    
    // 3. Migrate users
    logger.info('Step 3: Migrating users');
    const usersData = await readLegacyData('users');
    const transformedUsers = [];
    
    for (const user of usersData) {
      try {
        const transformedUser = transformUser(user);
        transformedUsers.push(transformedUser);
        stats.processed.users++;
        
        // Cache user ID for later reference
        cache.users.set(user.sn[0], transformedUser.id);
      } catch (error) {
        logger.error(`Failed to transform user: ${user.sn?.[0]}`, error);
        stats.errors.users++;
      }
    }
    
    await insertData(client, 'users', transformedUsers);
    
    // 4. Migrate items
    logger.info('Step 4: Migrating items');
    const itemsData = await readLegacyData('items');
    const transformedItems = [];
    
    for (const item of itemsData) {
      try {
        const transformedItem = transformItem(item);
        transformedItems.push(transformedItem);
        stats.processed.items++;
        
        // Cache item ID for later reference
        cache.items.set(item.sn[0], transformedItem.id);
      } catch (error) {
        logger.error(`Failed to transform item: ${item.sn?.[0]}`, error);
        stats.errors.items++;
      }
    }
    
    await insertData(client, 'items', transformedItems);
    
    // 5. Migrate boxes
    logger.info('Step 5: Migrating boxes');
    const boxesData = await readLegacyData('boxes');
    const transformedBoxes = [];
    
    for (const box of boxesData) {
      try {
        const transformedBox = transformBox(box);
        transformedBoxes.push(transformedBox);
        stats.processed.boxes++;
        
        // Cache box ID for later reference
        cache.boxes.set(box.sn[0], transformedBox.id);
      } catch (error) {
        logger.error(`Failed to transform box: ${box.sn?.[0]}`, error);
        stats.errors.boxes++;
      }
    }
    
    await insertData(client, 'boxes', transformedBoxes);
    
    // 6. Migrate orders
    logger.info('Step 6: Migrating orders');
    const ordersData = await readLegacyData('orders');
    const transformedOrders = [];
    
    for (const order of ordersData) {
      try {
        const transformedOrder = transformOrder(order);
        transformedOrders.push(transformedOrder);
        stats.processed.orders++;
      } catch (error) {
        logger.error(`Failed to transform order: ${order.sn?.[0]}`, error);
        stats.errors.orders++;
      }
    }
    
    await insertData(client, 'orders', transformedOrders);
    
    // 7. Migrate dictionaries
    logger.info('Step 7: Migrating dictionaries');
    const dictsData = await readLegacyData('dicts');
    const transformedDicts = [];
    
    for (const dict of dictsData) {
      try {
        const transformedDict = transformDict(dict);
        transformedDicts.push(transformedDict);
        stats.processed.dicts++;
      } catch (error) {
        logger.error(`Failed to transform dictionary`, error);
        stats.errors.dicts++;
      }
    }
    
    await insertTranslations(client, transformedDicts);
    
    // 8. Initialize serial counters
    logger.info('Step 8: Initializing serial counters');
    await initializeSerialCounters(client);
    
    logger.info('Migration completed successfully');
  } catch (error) {
    logger.error('Migration failed', error);
  } finally {
    // Close database connection
    if (client) {
      await client.end();
    }
    
    // Print statistics
    printStats();
  }
}

// Run the migration
migrate().catch(error => {
  console.error('Unhandled error in migration process:', error);
  process.exit(1);
});