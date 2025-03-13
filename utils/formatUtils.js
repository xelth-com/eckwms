// utils/formatUtils.js

/**
 * Split street address into street name and house number
 * @param {string} address - Full street address
 * @returns {Object} - Object with street and houseNumber properties
 */
function splitStreetAndHouseNumber(address) {
    try {
        if (typeof address !== "string") {
            throw new Error("Address must be a string");
        }

        // Updated regex to support different address formats
        const regex = /^(.*?)(\d{1,5}[A-Za-z\-\/\s]*)$/;

        const match = address.trim().match(regex);

        if (match) {
            const street = match[1].trim();
            const houseNumber = match[2].trim();

            return { street, houseNumber };
        } else {
            // No match: return the entire address as street
            return { street: address.trim(), houseNumber: "" };
        }
    } catch (error) {
        console.error("Error while splitting address:", error.message);
        // Fallback: store everything as street
        return { street: address.trim(), houseNumber: "" };
    }
}

/**
 * Split postal address into postal code and city
 * @param {string} address - Postal address
 * @returns {Object} - Object with city and postalCode properties
 */
function splitPostalCodeAndCity(address) {
    const regex = /(\d{4,6})\s*([A-Za-z]*)/;
    const match = address.trim().match(regex);

    if (match) {
        const postalCode = match[1];
        const city = match[2] || address.replace(postalCode, '').trim();
        return { city, postalCode };
    } else {
        // If no valid match, treat the entire input as the city
        return { city: address.trim(), postalCode: '' };
    }
}

/**
 * Convert RMA form data to serial and description array
 * @param {Object} rmaJs - RMA form data
 * @returns {Array} - Array of [serial, description] pairs
 */
function convertToSerialDescriptionArray(rmaJs) {
    const result = [];

    // Loop through keys of the rmaJs object
    for (let key in rmaJs) {
        // Check if the key starts with "serial"
        if (key.startsWith('serial')) {
            const index = key.replace('serial', '');  // Extract number from serial1, serial2, etc.
            const serial = rmaJs[key];  // Serial number
            const descriptionKey = `description${index}`;  // Corresponding description key
            const description = rmaJs[descriptionKey];  // Description

            if (serial && description) {
                result.push([serial, description]);  // Add the pair to the result array
            }
        }
    }

    return result;
}

/**
 * Format Unix timestamp to readable date and time
 * @param {number} timestamp - Unix timestamp
 * @returns {string} - Formatted date
 */
function formatUnixTimestamp(timestamp) {
    const date = new Date(parseInt(timestamp, 10) * 1000);
    const year = date.getUTCFullYear();
    const month = String(date.getUTCMonth() + 1).padStart(2, '0');
    const day = String(date.getUTCDate()).padStart(2, '0');
    const hours = String(date.getUTCHours()).padStart(2, '0');
    const minutes = String(date.getUTCMinutes()).padStart(2, '0');
    const seconds = String(date.getUTCSeconds()).padStart(2, '0');
    return `${year}-${month}-${day} ${hours}:${minutes}:${seconds}`;
}

/**
 * Mask certain fields in an object (for privacy/security)
 * @param {Object} obj - Object to mask fields in
 * @param {Array} fieldsToMask - Array of field names to mask
 * @returns {Object} - Object with masked fields
 */
function maskObjectFields(obj, fieldsToMask) {
    function maskString(str) {
        return str
            .split(' ')
            .map(word => (word.length > 1 ? word[0] + '*'.repeat(word.length - 1) : word))
            .join(' ');
    }

    function recursiveMask(obj) {
        const newObj = Array.isArray(obj) ? [] : {};
        for (const key in obj) {
            if (Object.prototype.hasOwnProperty.call(obj, key)) {
                const value = obj[key];
                if (fieldsToMask.includes(key) && typeof value === 'string') {
                    newObj[key] = maskString(value);
                } else if (typeof value === 'object' && value !== null) {
                    newObj[key] = recursiveMask(value);
                } else {
                    newObj[key] = value;
                }
            }
        }
        return newObj;
    }

    return recursiveMask(obj);
}

/**
 * Format an object for pretty HTML display
 * @param {Object} obj - Object to format
 * @returns {string} - HTML string with formatted object
 */
function prettyPrintObject(obj) {
    const json = JSON.stringify(obj, null, 2);

    const coloredJson = json.replace(
        /("(.*?)": )|("(.*?)")|(\b\d{10}\b)|(\b\d+\b)|\b(true|false|null)\b/g,
        (match, key, keyContent, string, stringContent, unixTimestamp, number, boolNull) => {
            if (key) {
                return `<span style="color: brown;">${key}</span>`;
            }
            if (string) {
                return `<span style="color: green;">${string}</span>`;
            }
            if (unixTimestamp) {
                const formattedDate = formatUnixTimestamp(unixTimestamp);
                return `<span style="color: purple;">"${formattedDate}"</span>`;
            }
            if (number) {
                return `<span style="color: orange;">${number}</span>`;
            }
            if (boolNull) {
                return `<span style="color: blue;">${boolNull}</span>`;
            }
            return match;
        }
    );

    return `<pre style="background-color: #f4f4f4; padding: 10px; border-radius: 5px; font-family: monospace; white-space: pre-wrap;">${coloredJson}</pre>`;
}

/**
 * Convert date string to Unix timestamp
 * @param {string} dateString - Date in format DD/MM/YYYY
 * @returns {number|null} - Unix timestamp or null if invalid
 */
function dateToUnix(dateString) {
    try {
        // Parse date with `DD/MM/YYYY` format
        const [day, month, year] = dateString.split('/').map(Number);

        if (!day || !month || !year) {
            throw new Error("Invalid date format. Use DD/MM/YYYY.");
        }

        // Convert to Date object
        const date = new Date(year, month - 1, day); // Month is 0-based

        if (isNaN(date.getTime())) {
            throw new Error("Invalid date value.");
        }

        // Unix timestamp in seconds
        return Math.floor(date.getTime() / 1000);
    } catch (error) {
        console.error("Error converting date to Unix time:", error.message);
        return null;
    }
}

module.exports = {
    splitStreetAndHouseNumber,
    splitPostalCodeAndCity,
    convertToSerialDescriptionArray,
    formatUnixTimestamp,
    maskObjectFields,
    prettyPrintObject,
    dateToUnix
};