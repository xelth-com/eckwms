/**
 * Fetch Utilities
 * Handles API interactions and form submissions
 */

/**
 * Performs an AJAX request to the server
 * @param {string} sendText - Text to send
 * @param {string} senderName - Sender name/identifier
 * @param {string} destination - Target element ID to update with response
 * @param {string} sendJWT - JWT token for authentication
 * @param {string} url - URL to send the request to
 * @returns {boolean} - Always returns false to prevent default form submission
 */
export function myFetch(sendText = '', senderName = '', destination = 'output1', sendJWT = localStorage.getItem("jwt"), url = '/') {
  let formData = {};
  if (sendText === 'formSubmit') {
    // Collect form data to JSON
    const elements = Array.from(document.getElementById(senderName).querySelectorAll('input, textarea, select'));
    elements.forEach(element => {
      if (element.value) {
        formData[element.id] = element.value;
      }
    });
    sendText = JSON.stringify(formData);
  }

  fetch(url, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json'
    },
    body: JSON.stringify({ text: sendText, name: senderName, dest: destination, jwt: sendJWT })
  })
    .then(async response => {
      if (!response.ok) {
        throw new Error('Network error: ' + response.statusText);
      }

      let blob;
      if (destination === 'pdfRma') {
        // PDF handling logic would go here
        console.log('PDF handling not implemented');
      } else if (destination === 'csv') {
        // CSV handling logic would go here
        console.log('CSV handling not implemented');
      } else {
        blob = await response.text();
        if (senderName == 'snInput' && sendText.split('.').length == 3) {
          const [encodedHeader, encodedPayload, signature] = sendText.split('.');
          localStorage.setItem("jwt", blob);
          myFetch('', 'startInput');
        }

        // Insert HTML into specified element
        const targetElement = document.getElementById(destination);
        if (targetElement) {
          targetElement.innerHTML = blob;

          // Update translations if available
          if (window.i18n && window.i18n.getCurrentLanguage() !== 'en') {
            window.i18n.updatePageTranslations();

            // Synchronize language UI with current language
            if (window.i18n.syncLanguageMasks) {
              window.i18n.syncLanguageMasks();
            } else {
              // Fallback synchronization if syncLanguageMasks isn't available
              const currentLang = window.i18n.getCurrentLanguage();
              document.documentElement.lang = currentLang;

              // Update mask states for language buttons
              const supportedLangs = ['de', 'en', 'fr', 'pl', 'cs', 'ko'];
              supportedLangs.forEach(lang => {
                const maskElement = document.getElementById(`${lang}Mask`);
                if (maskElement) {
                  maskElement.setAttribute("mask", "url(#maskClose)");
                }
              });

              // Set current language mask to open
              const currentMask = document.getElementById(`${currentLang}Mask`);
              if (currentMask) {
                currentMask.setAttribute("mask", "url(#maskOpen)");
              }
            }
          }

          // Check if this is for the RMA form
          if (senderName === 'rmaButton' && url.includes('/rma/generate')) {
            // Lazy load the RMA form script
            lazyLoadScript('/js/rma-form.js');
          }
        } else {
          console.error(`Target element not found: ${destination}`);
        }
      }
    })
    .catch(error => {
      console.error('Fetch operation problem:', error);
    });

  return false;
}

/**
 * Lazy-loads a script
 * @param {string} src - Script source path
 * @returns {Promise} - Promise that resolves when script is loaded
 */
export function lazyLoadScript(src) {
  // Check if script is already loaded
  if (document.querySelector(`script[src="${src}"]`)) {
    return Promise.resolve(); // Script already loaded
  }

  // Create script element
  const script = document.createElement('script');
  script.src = src;
  script.defer = true; // Defer execution until document is loaded

  // Return Promise to track loading
  return new Promise((resolve, reject) => {
    script.onload = () => {
      console.log(`Script ${src} loaded successfully`);
      resolve();
    };
    script.onerror = (error) => {
      console.error(`Error loading script ${src}`);
      reject(error);
    };

    // Add script to document
    document.body.appendChild(script);
  });
}

/**
 * Converts input string safely to integer
 * @param {string} input - Input string to convert
 * @returns {number} - Parsed integer or 0 if no valid integer found
 */
export function safeParseInt(input) {
  let match = input.match(/^\d+/);
  if (match) {
    return parseInt(match[0], 10);
  }
  return 0;
}

/**
 * Handle focus event for numeric inputs
 * @param {string} editId - ID of the element to focus
 */
export function onFocusInt(editId) {
  const myEl = document.getElementById(editId);
  if (myEl.innerText === '0') myEl.innerText = '';
}

/**
 * Handle focus out event for numeric inputs with validation
 * @param {string} editId - ID of the element to handle
 * @param {number} maxInt - Maximum allowed integer value
 */
export function onFocusOutInt(editId, maxInt = 0) {
  const myEl = document.getElementById(editId);
  
  myEl.innerText = safeParseInt(myEl.innerText);
  
  if (myEl.innerText > maxInt) {
    myEl.style.color = '#822';
  } else {
    myEl.style.color = '#072';
  }
}

/**
 * Set a random image from a provided array
 * @param {string} editId - ID of the image element
 * @param {string[]} picsArray - Array of image names to choose from
 */
export function picFromSet(editId, picsArray = []) {
  const myEl = document.getElementById(editId);
  if (picsArray.length && myEl.getAttribute('src') === '') {
    const ind = Math.floor(Math.random() * picsArray.length);
    myEl.setAttribute('src', `/storage/pics/${picsArray[ind]}.webp`);
    myEl.setAttribute('onclick', `fsPic('/storage/pics/${picsArray[ind]}.avif')`);
  }
}

// Add these functions to the window object for inline handlers
window.safeParseInt = safeParseInt;
window.onFocusInt = onFocusInt;
window.onFocusOutInt = onFocusOutInt;
