// utils/dataInit.js
const { readLinesToMap } = require('./fileUtils');
const { resolve } = require('path');

/**
 * Initializes data by loading from files
 * @param {string} mainDirectory - Base directory of the application
 * @returns {Promise<void>}
 */
async function initialisation(mainDirectory) {
    try {
        /* Uncomment if needed to create new classes from data
        // This part is for making new classes from some data
        const out = [];
        const files = await readdir(resolve(`${mainDirectory}classes/M3/`));
        files.forEach((element, index) => {
            const filePath = resolve(`${mainDirectory}classes/M3/${element}`);
            out.push(readFile(filePath, { encoding: 'utf8' }));
        });

        (await Promise.all(out)).forEach((prArr) => {
            JSON.parse(prArr).forEach((classDefenition, index) => {
                if (classDefenition.pn[0][1] === 'm3' && classDefenition.pn[0][0].length < 15) {
                    Object.setPrototypeOf(classDefenition, global.item)
                    classDefenition.cl = classDefenition.pn[0][0];
                    global.classes.set(classDefenition.pn[0][0], classDefenition)
                }
            });
        });
        */
        
        await readLinesToMap(global.users, resolve(`${mainDirectory}base/users.json`), global.user);
        await readLinesToMap(global.orders, resolve(`${mainDirectory}base/orders.json`), global.order);
        await readLinesToMap(global.uppers, resolve(`${mainDirectory}base/uppers.json`), global.item);
        await readLinesToMap(global.classes, resolve(`${mainDirectory}base/classes.json`), global.item);
        await readLinesToMap(global.items, resolve(`${mainDirectory}base/items.json`), global.item);
        await readLinesToMap(global.boxes, resolve(`${mainDirectory}base/boxes.json`), global.box);
        await readLinesToMap(global.places, resolve(`${mainDirectory}base/places.json`), global.place);
        await readLinesToMap(global.dicts, resolve(`${mainDirectory}base/dicts.json`), global.dict);
    } catch (err) {
        console.error(err.message);
    }
}

/**
 * Updates class relationships based on items
 */
function classesUpdate() {
    for (const [key, value] of global.items) {
        if (value.cl && global.classes.has(value.cl)) {
            addUnicEntryToProperty(global.classes, value.cl, ['i', value.sn[0]], 'down');
        }
    }
}

/**
 * Updates upper-level relationships
 */
function upperUpdate() {
    for (const [key, value] of global.classes) {
        value.rel.map(el => {
            if (el[0] == 'partOf') {
                if (!global.uppers.has(el[1])) {
                    global.uppers.set(el[1], Object.create(global.item));
                    global.uppers.get(el[1]).cl = el[1];
                }
                addUnicEntryToProperty(global.classes, value.cl, ['u', el[1]], 'up');
                addUnicEntryToProperty(global.uppers, el[1], ['c', value.cl], 'down');
            }
        });
    }
}

/**
 * Adds a unique entry to a property of an object within a Map.
 * @param {Map} collection - The Map containing the objects.
 * @param {string} targetKey - Key used to search the Map.
 * @param {Array} inputValue - Value to add.
 * @param {string} targetProperty - Property where to add the value.
 */
function addUnicEntryToProperty(collection, targetKey, inputValue, targetProperty) {
    const targetItem = collection.get(disAct(targetKey));
    if (targetItem) {
        if (!Object.hasOwn(targetItem, targetProperty)) {
            targetItem[targetProperty] = [];
        }

        if (!targetItem[targetProperty].reduce((akk, cur) => akk || (cur[0] === inputValue[0] && cur[1] === inputValue[1]), false)) {
            targetItem[targetProperty].push(inputValue);
        }
    }
}

/**
 * Adds a value to a specific property of an object within a Map.
 * @param {Map} collection - The Map containing the objects.
 * @param {string} targetKey - Key used to search the Map.
 * @param {any} inputValue - Value to add.
 * @param {string} targetProperty - Property where to add the value.
 */
function addEntryToProperty(collection, targetKey, inputValue, targetProperty) {
    // Check if the collection is a Map
    if (!(collection instanceof Map)) {
        throw new TypeError("The collection parameter must be a Map.");
    }

    // Retrieve the target object from the Map
    const targetItem = collection.get(disAct(targetKey));
    if (!targetItem) {
        console.warn(`Key '${targetKey}' not found in the Map.`);
        return;
    }

    // Ensure the target property exists and is an array
    if (!Object.hasOwn(targetItem, targetProperty)) {
        targetItem[targetProperty] = [];
    } else if (!Array.isArray(targetItem[targetProperty])) {
        throw new TypeError(`Property '${targetProperty}' is not an array on the target object.`);
    }

    // Add the input value to the array
    targetItem[targetProperty].push(inputValue);
}

/**
 * Converts first character to lowercase
 * @param {string} str - Input string
 * @returns {string} - String with first character lowercase
 */
function disAct(str) {
    return str.charAt(0).toLowerCase() + str.slice(1);
}

/**
 * Converts first character to uppercase
 * @param {string} str - Input string
 * @returns {string} - String with first character uppercase
 */
function toAct(str) {
    return str.charAt(0).toUpperCase() + str.slice(1);
}

/**
 * Checks if first character is uppercase
 * @param {string} str - Input string
 * @returns {boolean} - True if first character is uppercase
 */
function isAct(str) {
    return /^[A-Z]/.test(str.charAt(0));
}

/**
 * Finds a known code based on barcode
 * @param {string} barcode - Input barcode
 * @returns {string|boolean} - Known code or false
 */
function findKnownCode(barcode) {
    if (/^\d{7}$/.test(barcode)) return ('i70000000000' + (barcode));
    return false;
}

/**
 * Checks if input is a direct Eck code
 * @param {string} barcode - Input barcode
 * @returns {string|boolean} - Valid barcode or false
 */
function isBetDirect(barcode) {
    // Check if barcode is a string and 19 characters long
    if (typeof barcode === 'string' && barcode.length === 19) {
        // Check if barcode starts with one of the valid characters
        if (/^[ibpou]/.test(barcode)) {
            return barcode; // Valid barcode, return it
        }
    }
    return false; // Conditions not met
}

module.exports = {
    initialisation,
    classesUpdate,
    upperUpdate,
    addUnicEntryToProperty,
    addEntryToProperty,
    disAct,
    toAct,
    isAct,
    findKnownCode,
    isBetDirect
};