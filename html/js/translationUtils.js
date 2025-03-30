// html/js/translationUtils.js
// Improved with proper MD5 implementation to match server-side hashing

/**
 * A more accurate MD5 implementation for client-side use
 * Based on https://css-tricks.com/snippets/javascript/javascript-md5/
 * @param {string} input - String to hash
 * @returns {string} - MD5 hash
 */
function md5(input) {
    // Constants for MD5 algorithm
    const s = [
      7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22,
      5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20,
      4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23,
      6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21
    ];
    
    const K = [
      0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee,
      0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501,
      0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be,
      0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
      0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa,
      0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
      0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
      0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
      0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c,
      0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
      0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05,
      0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
      0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039,
      0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
      0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1,
      0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391
    ];
    
    // Convert string to UTF-8 bytes
    function utf8Encode(str) {
      let bytes = [];
      for (let i = 0; i < str.length; i++) {
        let charCode = str.charCodeAt(i);
        if (charCode < 0x80) {
          bytes.push(charCode);
        } else if (charCode < 0x800) {
          bytes.push(0xc0 | (charCode >> 6),
                     0x80 | (charCode & 0x3f));
        } else if (charCode < 0xd800 || charCode >= 0xe000) {
          bytes.push(0xe0 | (charCode >> 12),
                     0x80 | ((charCode >> 6) & 0x3f),
                     0x80 | (charCode & 0x3f));
        } else {
          // UTF-16 encode
          i++;
          charCode = 0x10000 + (((charCode & 0x3ff) << 10) | (str.charCodeAt(i) & 0x3ff));
          bytes.push(0xf0 | (charCode >> 18),
                     0x80 | ((charCode >> 12) & 0x3f),
                     0x80 | ((charCode >> 6) & 0x3f),
                     0x80 | (charCode & 0x3f));
        }
      }
      return bytes;
    }
    
    // Helper functions
    function leftRotate(x, c) {
      return (x << c) | (x >>> (32 - c));
    }
    
    function toHexString(num) {
      let hex = "";
      for (let i = 0; i < 4; i++) {
        const byte = (num >>> (i * 8)) & 0xff;
        hex += (byte < 16 ? '0' : '') + byte.toString(16);
      }
      return hex;
    }
    
    // Convert string to bytes and pad
    const bytes = utf8Encode(input);
    const originalBytesLen = bytes.length;
    
    // Padding: append the bit 1, then append 0 bits until length ≡ 448 (mod 512)
    bytes.push(0x80);
    const paddingLength = (512 - ((bytes.length * 8 + 64) % 512)) / 8;
    for (let i = 0; i < paddingLength; i++) {
      bytes.push(0);
    }
    
    // Append original length as a 64-bit number
    const bitLen = originalBytesLen * 8;
    for (let i = 0; i < 8; i++) {
      bytes.push((bitLen >>> (i * 8)) & 0xff);
    }
    
    // Initialize state (ABCD)
    let a0 = 0x67452301;
    let b0 = 0xefcdab89;
    let c0 = 0x98badcfe;
    let d0 = 0x10325476;
    
    // Process message in 64-byte chunks
    for (let i = 0; i < bytes.length; i += 64) {
      // Break chunk into 16 32-bit words
      const M = new Array(16);
      for (let j = 0; j < 16; j++) {
        M[j] = bytes[i + j*4] | (bytes[i + j*4 + 1] << 8) | 
               (bytes[i + j*4 + 2] << 16) | (bytes[i + j*4 + 3] << 24);
      }
      
      // Initialize hash values for this chunk
      let A = a0;
      let B = b0;
      let C = c0;
      let D = d0;
      
      // Main loop
      for (let j = 0; j < 64; j++) {
        let F, g;
        
        if (j < 16) {
          F = (B & C) | ((~B) & D);
          g = j;
        } else if (j < 32) {
          F = (D & B) | ((~D) & C);
          g = (5 * j + 1) % 16;
        } else if (j < 48) {
          F = B ^ C ^ D;
          g = (3 * j + 5) % 16;
        } else {
          F = C ^ (B | (~D));
          g = (7 * j) % 16;
        }
        
        // Update
        F = F + A + K[j] + M[g];
        A = D;
        D = C;
        C = B;
        B = B + leftRotate(F, s[j]);
      }
      
      // Add chunk's hash to result
      a0 = (a0 + A) & 0xffffffff;
      b0 = (b0 + B) & 0xffffffff;
      c0 = (c0 + C) & 0xffffffff;
      d0 = (d0 + D) & 0xffffffff;
    }
    
    // Convert to hex string
    return toHexString(a0) + toHexString(b0) + toHexString(c0) + toHexString(d0);
  }
  
  /**
   * Removes BOM from string
   * @param {string} text - Original text
   * @returns {string} - Text without BOM
   */
  function stripBOM(text) {
    if (text && typeof text === 'string' && text.charCodeAt(0) === 0xFEFF) {
      return text.slice(1);
    }
    return text;
  }
  
  /**
   * Generates a cache key for translation using the same algorithm as the server
   * @param {string} text - Original text to translate
   * @param {string} language - Target language
   * @param {string} namespace - Translation namespace
   * @param {Object} options - Additional options affecting the translation
   * @returns {string} - MD5 hash for cache key
   */
  function generateTranslationKey(text, language, namespace = '', options = {}) {
    // Remove BOM before generating the key
    const cleanText = stripBOM(text);
    
    // Create base key string
    let keyString = `${cleanText}_${namespace}_${language}`;
    
    // Add options to key if they affect the translation
    if (options.count !== undefined) {
      keyString += `_count=${options.count}`;
    }
    
    // Add any future option parameters that affect translation output
    // if (options.gender) keyString += `_gender=${options.gender}`;
    // if (options.context) keyString += `_context=${options.context}`;
    
    // Generate a proper MD5 hash that matches the server
    return md5(keyString);
  }
  
  // Export functions
  window.translationUtils = {
    generateTranslationKey,
    stripBOM,
    md5 // Export for testing purposes
  };