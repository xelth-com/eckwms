// utils/bomUtils.js
/**
 * Utility functions for handling BOM (Byte Order Mark) in text files
 */

/**
 * Removes UTF-8 BOM from a string
 * @param {string} content - Content to process
 * @returns {string} - Content without BOM
 */
function stripBOM(content) {
  if (content && typeof content === 'string' && content.charCodeAt(0) === 0xFEFF) {
    return content.slice(1);
  }
  return content;
}

/**
 * Safely parses JSON with BOM handling
 * @param {string} content - JSON string to parse
 * @returns {Object} - Parsed JSON object
 */
function parseJSONWithBOM(content) {
  if (!content) return null;
  try {
    // First strip BOM if present
    const cleanContent = stripBOM(content);
    return JSON.parse(cleanContent);
  } catch (error) {
    console.error('Error parsing JSON:', error);
    throw new Error(`Failed to parse JSON: ${error.message}`);
  }
}

/**
 * Safely reads and parses a JSON file with BOM handling
 * @param {string} filePath - Path to JSON file
 * @param {Object} fs - File system module
 * @returns {Promise<Object>} - Parsed JSON object
 */
async function readJSONWithBOM(filePath, fs) {
  try {
    const content = await fs.promises.readFile(filePath, 'utf8');
    return parseJSONWithBOM(content);
  } catch (error) {
    console.error(`Error reading file ${filePath}:`, error);
    throw error;
  }
}

/**
 * Synchronously reads and parses a JSON file with BOM handling
 * @param {string} filePath - Path to JSON file
 * @param {Object} fs - File system module
 * @returns {Object} - Parsed JSON object
 */
function readJSONWithBOMSync(filePath, fs) {
  try {
    const content = fs.readFileSync(filePath, 'utf8');
    return parseJSONWithBOM(content);
  } catch (error) {
    console.error(`Error reading file ${filePath}:`, error);
    throw error;
  }
}

module.exports = {
  stripBOM,
  parseJSONWithBOM,
  readJSONWithBOM,
  readJSONWithBOMSync
};