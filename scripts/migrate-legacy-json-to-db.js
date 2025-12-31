require('dotenv').config();
const fs = require('fs');
const path = require('path');
const readline = require('readline');
const db = require('../src/shared/models/postgresql');

async function processFile(filename, modelName, type) {
    const filePath = path.join(__dirname, '../base', filename);
    if (!fs.existsSync(filePath)) {
        console.log(`Skipping ${filename} (not found)`);
        return;
    }
    console.log(`Processing ${filename}...`);

    const fileStream = fs.createReadStream(filePath);
    const rl = readline.createInterface({ input: fileStream, crlfDelay: Infinity });

    let count = 0;
    const records = [];

    for await (const line of rl) {
        try {
            const obj = JSON.parse(line);
            let id = obj.sn ? obj.sn[0] : null;

            // Fallback for objects without sn array
            if (!id && obj.cl) id = obj.cl;
            if (!id) continue;

            records.push({
                id: id,
                class: obj.cl || null,
                data: obj,
                createdAt: obj.sn ? new Date(obj.sn[1] * 1000) : new Date(),
                updatedAt: new Date()
            });
            count++;

            if (records.length >= 1000) {
                await db[modelName].bulkCreate(records, { updateOnDuplicate: ['data', 'updatedAt'] });
                records.length = 0;
                process.stdout.write(`.`);
            }
        } catch (e) { console.error('Parse error:', e.message); }
    }

    if (records.length > 0) {
        await db[modelName].bulkCreate(records, { updateOnDuplicate: ['data', 'updatedAt'] });
    }
    console.log(`\nDone! Imported ${count} ${type}s.`);
}

async function run() {
    await db.sequelize.authenticate();
    await db.sequelize.sync();

    await processFile('items.json', 'Item', 'item');
    await processFile('boxes.json', 'Box', 'box');
    await processFile('places.json', 'Place', 'place');

    console.log('Migration complete.');
    process.exit(0);
}

run();
