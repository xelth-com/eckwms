// File: html/js/rma-form.js

(function () {
  // Current device counter
  let deviceCount = 0;
  let i18nInitialized = false;

  // Initialization function
  function init() {
    // Check i18n status and set up initialization
    setupI18n();

    // Set up add device button
    const addButton = document.getElementById('add-device-btn');
    if (addButton) {
      addButton.addEventListener('click', function () {
        addDeviceEntry();
      });
    }

    // Set up form handler
    setupFormHandler();

    // Add address logic explanation
    addAddressLogicExplanation();
    
    // Add first device entry only if i18n is ready,
    // otherwise wait for the i18n initialization event
    if (i18nInitialized || 
        (window.i18n && window.i18n.isInitialized()) || 
        (window.i18n && window.i18n.getCurrentLanguage() === 'en')) {
      addDeviceEntry();
    }
  }

  /**
   * Set up i18n and handle initialization
   */
  function setupI18n() {
    // If i18n is not defined, load it
    if (typeof window.i18n === 'undefined') {
      console.log("i18n not loaded, loading now...");
      loadI18nScript();
      
      // Listen for i18n initialization event
      document.addEventListener('i18n:initialized', function() {
        console.log("i18n:initialized event received");
        i18nInitialized = true;
        // Now that i18n is ready, add the first device entry if none exist yet
        if (deviceCount === 0) {
          addDeviceEntry();
        }
        
        // Translate any existing elements
        translateAllElements();
      });
    } else {
      // i18n is already loaded
      console.log("i18n already loaded, checking if initialized");
      if (window.i18n.isInitialized()) {
        console.log("i18n is already initialized");
        i18nInitialized = true;
      } else {
        console.log("i18n is loaded but not yet initialized, waiting for event");
        // i18n is loaded but not initialized, wait for it
        document.addEventListener('i18n:initialized', function() {
          console.log("i18n:initialized event received");
          i18nInitialized = true;
          // Add the first device entry if none exist yet
          if (deviceCount === 0) {
            addDeviceEntry();
          }
          
          // Translate any existing elements
          translateAllElements();
        });
      }
    }
  }

  /**
   * Translate all elements with i18n tags
   */
  function translateAllElements() {
    if (window.i18n && window.i18n.getCurrentLanguage() !== 'en') {
      // Translate the entire form
      const form = document.getElementById('rmaForm');
      if (form) {
        window.i18n.translateDynamicElement(form);
      }
      
      // Specifically translate all device entries
      const deviceEntries = document.querySelectorAll('.device-entry');
      deviceEntries.forEach(entry => {
        window.i18n.translateDynamicElement(entry);
      });
      
      // Translate address logic explanation
      const addressInfo = document.querySelector('.address-logic-info');
      if (addressInfo) {
        window.i18n.translateDynamicElement(addressInfo);
      }
    }
  }

  /**
   * Loads the i18n script if it's not already loaded
   */
  function loadI18nScript() {
    if (document.querySelector('script[src="/js/i18n.js"]')) {
      return; // Script already loaded
    }

    // Load CSS
    if (!document.querySelector('link[href="/css/i18n.css"]')) {
      const linkElem = document.createElement('link');
      linkElem.rel = 'stylesheet';
      linkElem.href = '/css/i18n.css';
      document.head.appendChild(linkElem);
    }

    // Load JS
    const script = document.createElement('script');
    script.src = '/js/i18n.js';
    script.defer = true;
    script.onload = function () {
      if (window.i18n) {
        window.i18n.init();
        addLanguageSelector();
      }
    };

    document.body.appendChild(script);
  }

  /**
   * Adds a language selector to the form
   */
  function addLanguageSelector() {
    const rmaForm = document.getElementById('rmaForm');
    if (!rmaForm) return;

    // Create container for language selector
    const langContainer = document.createElement('div');
    langContainer.className = 'form-language-selector';
    langContainer.style.textAlign = 'right';
    langContainer.style.margin = '0 0 20px 0';

    // Create title
    const langTitle = document.createElement('span');
    langTitle.textContent = 'Sprache / Language: ';
    langTitle.setAttribute('data-i18n', 'common:language.select');
    langTitle.style.marginRight = '10px';

    // Create selector
    const langSelector = document.createElement('div');
    langSelector.className = 'language-selector';

    // Add to DOM
    langContainer.appendChild(langTitle);
    langContainer.appendChild(langSelector);

    // Insert before main content
    const firstChild = rmaForm.firstChild;
    rmaForm.insertBefore(langContainer, firstChild);
  }

  /**
   * Helper function to safely get a translation
   * This function checks if the translation key exists and returns a sensible fallback
   * @param {string} key - Translation key
   * @param {Object} options - Translation options
   * @param {string} fallback - Fallback text if key doesn't exist
   * @returns {string} - Translated text or fallback
   */
  function getTranslation(key, options = {}, fallback = '') {
    if (!window.i18n || !window.i18n.isInitialized()) {
      return fallback || key.split('.').pop(); // Just return the fallback or last part of key as text
    }
    
    // Check if key exists in translation files (testing existence, not returning value)
    let translation;
    try {
      // Try to get the translation
      translation = window.i18n.t(key, options);
      
      // If i18next returns the key itself, it means the key doesn't exist
      // Most i18n libraries return the key when a translation is missing
      if (translation === key || translation.includes('i18n key not found')) {
        console.log(`Translation key not found: ${key}`);
        return fallback || key.split('.').pop(); // Use fallback or extract readable part from key
      }
      
      return translation;
    } catch (error) {
      console.error(`Error getting translation for key: ${key}`, error);
      return fallback || key.split('.').pop();
    }
  }

  // Add address logic explanation
  function addAddressLogicExplanation() {
    const devicesContainer = document.getElementById('devices-container');
    if (devicesContainer) {
      const addressInfo = document.createElement('div');
      addressInfo.className = 'address-logic-info';
      addressInfo.style.padding = '10px';
      addressInfo.style.marginBottom = '20px';
      addressInfo.style.backgroundColor = '#e6f7ff';
      addressInfo.style.border = '1px solid #91d5ff';
      addressInfo.style.borderRadius = '4px';
      addressInfo.style.fontSize = '14px';

      addressInfo.innerHTML = `
  <p data-i18n="rma:address_logic.title"><strong>Device Return Information:</strong></p>
  <ul style="margin-top: 5px; padding-left: 20px;">
    <li data-i18n="rma:address_logic.info.0">By default, all devices are shipped to the address specified in the "Billing Information" section.</li>
    <li data-i18n="rma:address_logic.info.1">You can specify an alternative return address for any device.</li>
    <li data-i18n="rma:address_logic.info.2">If you haven't specified an address for a device, the nearest address above in the list will be used.</li>
  </ul>
`;

      devicesContainer.parentNode.insertBefore(addressInfo, devicesContainer);
      
      // Apply translations if i18n is available and initialized
      if (window.i18n && window.i18n.isInitialized() && window.i18n.getCurrentLanguage() !== 'en') {
        window.i18n.translateDynamicElement(addressInfo);
      }
    }
  }

  // Function to add a new device entry
  function addDeviceEntry() {
    deviceCount++;
    const deviceIndex = deviceCount;

    const deviceEntry = document.createElement('div');
    deviceEntry.className = 'device-entry';
    deviceEntry.dataset.index = deviceIndex;
    deviceEntry.style.border = '1px solid #ddd';
    deviceEntry.style.borderRadius = '4px';
    deviceEntry.style.padding = '15px';
    deviceEntry.style.marginBottom = '20px';
    deviceEntry.style.backgroundColor = '#f9f9f9';

    // Create device header with number
    const deviceHeader = document.createElement('div');
    deviceHeader.style.display = 'flex';
    deviceHeader.style.justifyContent = 'space-between';
    deviceHeader.style.alignItems = 'center';
    deviceHeader.style.marginBottom = '15px';

    const deviceTitle = document.createElement('h4');
    deviceTitle.setAttribute('data-i18n', 'rma:device.title');
    deviceTitle.setAttribute('data-i18n-options', JSON.stringify({count: deviceIndex}));
    deviceTitle.textContent = `Device #${deviceIndex}`;
    deviceTitle.style.margin = '0';
    deviceTitle.style.color = '#1e2071';

    // Toggle button for alternate shipping address
    const toggleButton = document.createElement('button');
    toggleButton.type = 'button';
    toggleButton.setAttribute('data-i18n', 'rma:device.address_button');
    toggleButton.textContent = 'Specify Different Return Address';
    toggleButton.style.backgroundColor = '#eee';
    toggleButton.style.border = '1px solid #ccc';
    toggleButton.style.borderRadius = '4px';
    toggleButton.style.padding = '5px 10px';
    toggleButton.style.cursor = 'pointer';
    toggleButton.dataset.deviceIndex = deviceIndex;

    deviceHeader.appendChild(deviceTitle);
    deviceHeader.appendChild(toggleButton);
    deviceEntry.appendChild(deviceHeader);

    // Create alternate shipping address section (initially hidden)
    const alternateAddressSection = document.createElement('div');
    alternateAddressSection.id = `alternate-address-${deviceIndex}`;
    alternateAddressSection.style.display = 'none';
    alternateAddressSection.style.padding = '10px';
    alternateAddressSection.style.border = '1px solid #ddd';
    alternateAddressSection.style.borderRadius = '4px';
    alternateAddressSection.style.marginBottom = '15px';
    alternateAddressSection.style.backgroundColor = '#fff';

    // Add alternate address fields
    alternateAddressSection.innerHTML = `
        <h4 style="margin-top: 0; color: #1e2071;" data-i18n="rma:device.alternate_address">Alternate Return Address</h4>
        <p style="font-style: italic; margin-bottom: 10px;" data-i18n="rma:device.address_info">Specify a different address for returning this device.</p>
        
        <label for="alt_company_${deviceIndex}" style="display: block; margin-top: 10px;"><b data-i18n="rma:form.company_name">Company Name:</b></label>
        <input type="text" id="alt_company_${deviceIndex}" name="alt_company_${deviceIndex}" 
               style="width: 95%; padding: 5px; font-size: 16px; background-color: #eee; margin-top: 5px;">
  
        <label for="alt_person_${deviceIndex}" style="display: block; margin-top: 10px;" data-i18n="rma:form.contact_person">Contact Person:</label>
        <input type="text" id="alt_person_${deviceIndex}" name="alt_person_${deviceIndex}" 
               style="width: 95%; padding: 5px; font-size: 16px; background-color: #eee; margin-top: 5px;">
  
        <label for="alt_street_${deviceIndex}" style="display: block; margin-top: 10px;"><b data-i18n="rma:form.street">Street and House Number:</b></label>
        <input type="text" id="alt_street_${deviceIndex}" name="alt_street_${deviceIndex}" 
               style="width: 95%; padding: 5px; font-size: 16px; background-color: #eee; margin-top: 5px;">
  
        <label for="alt_addressLine2_${deviceIndex}" style="display: block; margin-top: 10px;" data-i18n="rma:form.additional_address">Additional Address Line:</label>
  <input type="text" id="alt_addressLine2_${deviceIndex}" name="alt_addressLine2_${deviceIndex}" 
         style="width: 95%; padding: 5px; font-size: 16px; background-color: #eee; margin-top: 5px;">

  <label for="alt_postalCode_${deviceIndex}" style="display: block; margin-top: 10px;"><b data-i18n="rma:form.postal_code">Postal Code:</b></label>
  <input type="text" id="alt_postalCode_${deviceIndex}" name="alt_postalCode_${deviceIndex}" 
         style="width: 95%; padding: 5px; font-size: 16px; background-color: #eee; margin-top: 5px;">

  <label for="alt_city_${deviceIndex}" style="display: block; margin-top: 10px;"><b data-i18n="rma:form.city">City:</b></label>
  <input type="text" id="alt_city_${deviceIndex}" name="alt_city_${deviceIndex}" 
         style="width: 95%; padding: 5px; font-size: 16px; background-color: #eee; margin-top: 5px;">

        <label for="alt_country_${deviceIndex}" style="display: block; margin-top: 10px;"><b data-i18n="rma:form.country">Country:</b></label>
        <input type="text" id="alt_country_${deviceIndex}" name="alt_country_${deviceIndex}" value="Germany" 
               style="width: 95%; padding: 5px; font-size: 16px; background-color: #eee; margin-top: 5px;">
      `;

    deviceEntry.appendChild(alternateAddressSection);

    // Create serial number and description section
    const deviceDetailsSection = document.createElement('div');

    // Serial number input
    const serialLabel = document.createElement('label');
    serialLabel.htmlFor = `serial${deviceIndex}`;
    serialLabel.setAttribute('data-i18n', 'rma:device.serial_number');
    serialLabel.textContent = 'Serial Number:';
    serialLabel.style.display = 'block';
    serialLabel.style.marginTop = '10px';
    serialLabel.style.fontWeight = 'bold';

    const serialInput = document.createElement('input');
    serialInput.type = 'text';
    serialInput.id = `serial${deviceIndex}`;
    serialInput.name = `serial${deviceIndex}`;
    serialInput.setAttribute('data-i18n-attr', JSON.stringify({"placeholder": "rma:device.serial_placeholder"}));
    serialInput.style.width = '95%';
    serialInput.style.padding = '5px';
    serialInput.style.fontSize = '18px';
    serialInput.style.backgroundColor = '#eee';
    serialInput.style.marginTop = '5px';

    // Description textarea
    const descLabel = document.createElement('label');
    descLabel.htmlFor = `description${deviceIndex}`;
    descLabel.setAttribute('data-i18n', 'rma:device.issue_description');
    descLabel.textContent = 'Issue Description:';
    descLabel.style.display = 'block';
    descLabel.style.marginTop = '10px';
    descLabel.style.fontWeight = 'bold';

    const descTextarea = document.createElement('textarea');
    descTextarea.id = `description${deviceIndex}`;
    descTextarea.name = `description${deviceIndex}`;
    descTextarea.setAttribute('data-i18n-attr', JSON.stringify({"placeholder": "rma:device.description_placeholder"}));
    descTextarea.rows = 3;
    descTextarea.style.width = '95%';
    descTextarea.style.padding = '5px';
    descTextarea.style.fontSize = '18px';
    descTextarea.style.backgroundColor = '#eee';
    descTextarea.style.marginTop = '5px';

    deviceDetailsSection.appendChild(serialLabel);
    deviceDetailsSection.appendChild(serialInput);
    deviceDetailsSection.appendChild(descLabel);
    deviceDetailsSection.appendChild(descTextarea);

    deviceEntry.appendChild(deviceDetailsSection);

    // Add the complete device entry to the container
    deviceEntry.appendChild(deviceDetailsSection);

    // Add element to DOM
    const devicesContainer = document.getElementById('devices-container');
    if (devicesContainer) {
      devicesContainer.appendChild(deviceEntry);

      // Translate if i18n is initialized and language is not default
      if (window.i18n && window.i18n.isInitialized() && window.i18n.getCurrentLanguage() !== 'en') {
        window.i18n.translateDynamicElement(deviceEntry);
      }
    }

    // Set up event listeners for this device entry
    setupDeviceEventListeners(deviceIndex);

    // Display address source info
    updateAddressSourceInfo(deviceIndex);

    return deviceEntry;
  }

  // Display address source info - FIX THE ISSUE HERE
  function updateAddressSourceInfo(deviceIndex) {
    const deviceEntries = document.querySelectorAll('.device-entry');

    // Loop through all devices
    deviceEntries.forEach(entry => {
      const entryIndex = parseInt(entry.dataset.index, 10);

      // Skip current and previous devices
      if (entryIndex < deviceIndex) return;

      // Find nearest address source above
      let addressSource = findNearestAddressSource(entryIndex);
      const toggleBtn = entry.querySelector('button');

      if (!toggleBtn) return;

      // Check if alternate address section is open for this device
      const altAddressSection = document.getElementById(`alternate-address-${entryIndex}`);
      const hasAltAddress = altAddressSection && altAddressSection.style.display !== 'none';

      if (hasAltAddress) {
        // Device has its own address
        toggleBtn.style.backgroundColor = '#e6f7ff';
      } else if (addressSource === 0) {
        // Using billing address - FIX: Use getTranslation helper function
        toggleBtn.textContent = getTranslation(
          'rma:device.using_billing', 
          {}, 
          'Using Billing Address'
        );
        toggleBtn.style.backgroundColor = '#f0f0f0';
      } else {
        // Using another device's address - FIX: Use getTranslation helper function
        const translateOptions = {count: addressSource};
        toggleBtn.textContent = getTranslation(
          'rma:device.using_address', 
          translateOptions, 
          `Using Address from Device #${addressSource}`
        );
        toggleBtn.style.backgroundColor = '#f0f0f0';
      }
    });
  }

  // Find nearest address source above
  function findNearestAddressSource(deviceIndex) {
    // Check if this device has its own address
    const altAddressSection = document.getElementById(`alternate-address-${deviceIndex}`);
    if (altAddressSection && altAddressSection.style.display !== 'none') {
      return deviceIndex; // Device uses its own address
    }

    // Look for nearest device above with a specified address
    for (let i = deviceIndex - 1; i >= 1; i--) {
      const prevAltAddressSection = document.getElementById(`alternate-address-${i}`);
      if (prevAltAddressSection && prevAltAddressSection.style.display !== 'none') {
        return i; // Found nearest device with address
      }
    }

    // If nothing found, use billing address
    return 0;
  }

  // Set up event listeners for device
  function setupDeviceEventListeners(deviceIndex) {
    // Toggle button for alternate address
    const toggleButton = document.querySelector(`.device-entry[data-index="${deviceIndex}"] button`);
    if (toggleButton) {
      toggleButton.addEventListener('click', function () {
        const addressSection = document.getElementById(`alternate-address-${deviceIndex}`);
        if (addressSection) {
          if (addressSection.style.display === 'none') {
            addressSection.style.display = 'block';
            // FIX: Use getTranslation helper function
            toggleButton.textContent = getTranslation(
              'rma:device.hide_address', 
              {}, 
              'Hide Return Address'
            );
            toggleButton.style.backgroundColor = '#e6f7ff';

            // Translate the address section if it was just opened
            if (window.i18n && window.i18n.isInitialized() && window.i18n.getCurrentLanguage() !== 'en') {
              window.i18n.translateDynamicElement(addressSection);
            }
          } else {
            addressSection.style.display = 'none';
            // FIX: Use getTranslation helper function
            toggleButton.textContent = getTranslation(
              'rma:device.address_button', 
              {}, 
              'Specify Different Return Address'
            );
            toggleButton.style.backgroundColor = '#eee';
          }

          // Update address info for all devices
          for (let i = 1; i <= deviceCount; i++) {
            updateAddressSourceInfo(i);
          }
        }
      });
    }

    // When description field is focused, add new device entry if this is the last one
    const descTextarea = document.getElementById(`description${deviceIndex}`);
    const serialInput = document.getElementById(`serial${deviceIndex}`);

    if (descTextarea && serialInput) {
      descTextarea.addEventListener('focus', function () {
        if (deviceIndex === deviceCount && serialInput.value.trim() !== '') {
          addDeviceEntry();
        }
      });

      // Also check after typing in the serial field
      serialInput.addEventListener('input', function () {
        if (deviceIndex === deviceCount && this.value.trim() !== '') {
          // Check if the description already has focus
          if (document.activeElement !== descTextarea) {
            // If not, we won't add a new entry yet - wait for description focus
          }
        }
      });
    }
  }

  // Set up form handler
  function setupFormHandler() {
    const rmaForm = document.getElementById('rmaForm');
    if (rmaForm) {
      rmaForm.addEventListener('submit', function (e) {
        // This is handled by myFetch function, no need to preventDefault()

        // Add metadata about return addresses as hidden fields
        document.querySelectorAll('.device-entry').forEach(entry => {
          const index = entry.dataset.index;
          const alternateAddressSection = document.getElementById(`alternate-address-${index}`);

          // If this device has an alternate address specified and it's visible
          if (alternateAddressSection && alternateAddressSection.style.display !== 'none') {
            // Create a hidden field to indicate this device has an alternate address
            const hasAltAddressField = document.createElement('input');
            hasAltAddressField.type = 'hidden';
            hasAltAddressField.id = `has_alt_address_${index}`;
            hasAltAddressField.name = `has_alt_address_${index}`;
            hasAltAddressField.value = 'true';
            entry.appendChild(hasAltAddressField);

            // Create a hidden field for which address to use for this device
            const addressSourceField = document.createElement('input');
            addressSourceField.type = 'hidden';
            addressSourceField.id = `address_source_${index}`;
            addressSourceField.name = `address_source_${index}`;
            addressSourceField.value = index; // Use its own address
            entry.appendChild(addressSourceField);
          } else {
            // Find nearest address source above
            const sourceIndex = findNearestAddressSource(parseInt(index));

            // Create a hidden field for which address to use for this device
            const addressSourceField = document.createElement('input');
            addressSourceField.type = 'hidden';
            addressSourceField.id = `address_source_${index}`;
            addressSourceField.name = `address_source_${index}`;
            addressSourceField.value = sourceIndex;
            entry.appendChild(addressSourceField);
          }
        });
      });
    }
  }

  // Run initialization when DOM is fully loaded
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }
})();