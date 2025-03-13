// utils/fileUtils.js
const fs = require('fs');
const { resolve } = require('path');
const readline = require('readline');

/**
 * Writes a Map to a file asynchronously with each entry on a separate line
 * @param {Map} map - The Map to write to a file
 * @param {string} filePath - File path where to write data
 * @returns {Promise<string>} - Success message
 */
function writeLargeMapToFile(map, filePath) {
    return new Promise((resolve, reject) => {
        const writeStream = fs.createWriteStream(filePath);

        let firstEntry = true; 

        // Iterate over the map and write each element individually
        for (const [key, value] of map) {
            if (!firstEntry) {
                writeStream.write('\n');
            } else {
                firstEntry = false;
            }

            // Write the key-value pair as JSON
            writeStream.write(JSON.stringify(value));
        }

        // End the stream
        writeStream.end();

        // Event handling for success and errors
        writeStream.on('finish', () => {
            resolve(`File written successfully: ${filePath}`);
        });

        writeStream.on('error', (err) => {
            reject(`Error writing to file: ${err}`);
        });
    });
}

/**
 * Reads data from file and populates a Map
 * @param {Map} map - Map to populate
 * @param {string} filePath - File path to read from
 * @param {Object} cla - Class prototype to use
 * @returns {Promise<void>}
 */
async function readLinesToMap(map, filePath, cla) {
    try {
        // Check if file exists
        await fs.promises.access(filePath);

        return new Promise((resolve, reject) => {
            const readInterface = readline.createInterface({
                input: fs.createReadStream(filePath),
                console: false
            });

            readInterface.on('line', (line) => {
                try {
                    const jsonObj = JSON.parse(line);
                    if (Object.hasOwn(jsonObj, 'sn')) {
                        const key = jsonObj.sn[0];
                        if (key !== undefined) {
                            if (jsonObj.cl && global.classes.has(jsonObj.cl)) {
                                Object.setPrototypeOf(jsonObj, global.classes.get(jsonObj.cl));
                            } else {
                                Object.setPrototypeOf(jsonObj, cla);
                            }
                            map.set(key, jsonObj);
                        }
                    } else if (Object.hasOwn(jsonObj, 'cl')) {
                        const key = jsonObj.cl;
                        Object.setPrototypeOf(jsonObj, cla);
                        map.set(key, jsonObj);
                    } else if (Object.hasOwn(jsonObj, 'orig')) {
                        const key = jsonObj.orig;
                        Object.setPrototypeOf(jsonObj, cla);
                        map.set(key, jsonObj);
                    }
                } catch (err) {
                    console.error('Error processing line:', err.message);
                }
            });

            readInterface.on('close', () => {
                resolve();
            });

            readInterface.on('error', (err) => {
                reject(`Error reading file: ${err.message}`);
            });
        });
    } catch (err) {
        console.log(`File ${filePath} does not exist or cannot be opened.`);
    }
}

/**
 * Reads CSV file and converts to JSON
 * @param {string} filePath - Path to CSV file
 * @returns {Array} - Array of objects from CSV
 */
function readCsvFile(filePath) {
    try {
        // Read file content synchronously
        const csvContent = fs.readFileSync(filePath, "utf-8");
        return csvToJson(csvContent);
    } catch (error) {
        console.error(`Error reading the file ${filePath}:`, error.message);
        return [];
    }
}

/**
 * Converts CSV string to JSON
 * @param {string} csvString - CSV string to convert
 * @param {string} delimiter - Delimiter used in CSV (default: ';')
 * @returns {Array} - Array of objects
 */
function csvToJson(csvString, delimiter = ';') {
    try {
        if (typeof csvString !== "string") {
            throw new Error("Input must be a string");
        }

        const lines = csvString.trim().split("\n");
        if (lines.length < 2) {
            throw new Error("CSV must have at least a header and one data row");
        }

        const headers = lines[0].split(delimiter).map(header => header.trim());
        const jsonArray = lines.slice(1).map(line => {
            const values = line.split(delimiter).map(value => value.trim());
            if (values.length !== headers.length) {
                throw new Error("Row has a different number of columns than the header");
            }

            return headers.reduce((obj, header, index) => {
                obj[header] = values[index];
                return obj;
            }, {});
        });

        return jsonArray;
    } catch (error) {
        console.error("Error converting CSV to JSON:", error.message);
        return [];
    }
}

module.exports = {
    writeLargeMapToFile,
    readLinesToMap,
    readCsvFile,
    csvToJson
};