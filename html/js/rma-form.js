// html/js/rma-form.js - Улучшенная версия с явной инициализацией

(function() {
  // Use global config for default language instead of hardcoded value
  const defaultLanguage = window.APP_CONFIG?.DEFAULT_LANGUAGE || 'en';
  // Current device counter
  let deviceCount = 0;
  
  // Utility function for dev-only console logging
  function devLog(...args) {
    if (window.APP_CONFIG?.NODE_ENV === 'development') {
      console.log(...args);
    } else {
      console.log(...args); // временно включим логи для отладки
    }
  }

  /**
   * Получение перевода без i18n (для аварийных случаев)
   * @param {string} key - ключ перевода
   * @param {string} defaultText - текст по умолчанию
   * @returns {string} - переведенный текст или текст по умолчанию
   */
  function getTranslation(key, defaultText) {
    // Базовые переводы для немецкого языка
    const basicTranslations = {
      'rma:device.title': 'Gerät #${count}',
      'rma:device.address_button': 'Andere Rücksendeadresse angeben',
      'rma:device.serial_number': 'Seriennummer:',
      'rma:device.serial_placeholder': 'Seriennummer eingeben',
      'rma:device.issue_description': 'Problembeschreibung:',
      'rma:device.description_placeholder': 'Beschreiben Sie das Problem mit dem Gerät',
      'rma:device.using_billing': 'Rechnungsadresse wird verwendet'
    };

    // Если i18n доступен, используем его
    if (window.i18n && window.i18n.t) {
      return window.i18n.t(key, { defaultValue: defaultText });
    }
    
    // Проверка языка страницы
    const lang = document.documentElement.lang || 'en';
    if (lang === 'de' && basicTranslations[key]) {
      return basicTranslations[key].replace('${count}', 
        key === 'rma:device.title' ? deviceCount : '');
    }
    
    return defaultText;
  }

  /**
   * Initialization function
   */
  function init() {
    // Check DOM state
    if (document.readyState !== 'complete' && document.readyState !== 'interactive') {
      devLog("DOM not ready, waiting for load...");
      document.addEventListener('DOMContentLoaded', init);
      return;
    }
    
    devLog("RMA Form: начало инициализации");
    
    // Set up add device button
    const addButton = document.getElementById('add-device-btn');
    if (addButton) {
      addButton.addEventListener('click', function() {
        addDeviceEntry();
      });
    }

    // Set up form handler
    setupFormHandler();

    // Add address logic explanation
    addAddressLogicExplanation();
    
    // Add first device entry
    devLog("RMA Form: добавляем первый элемент устройства");
    addDeviceEntry();
    
    // Установим обработчик для явного обновления переводов
    if (window.i18n && typeof window.i18n.updatePageTranslations === 'function') {
      devLog("RMA Form: устанавливаем таймер для повторного обновления переводов");
      // Запустим вторую волну переводов через короткое время после загрузки
      setTimeout(function() {
        window.i18n.updatePageTranslations();
        
        // Попробуем также обновить атрибуты placeholder, которые могли не обновиться
        document.querySelectorAll('input[placeholder="null"], textarea[placeholder="null"]').forEach(elem => {
          const attr = elem.getAttribute('data-i18n-attr');
          if (attr) {
            try {
              const attrMap = JSON.parse(attr);
              if (attrMap.placeholder) {
                const key = attrMap.placeholder;
                elem.placeholder = getTranslation(key, 
                  elem.tagName === 'TEXTAREA' ? 'Describe the problem' : 'Enter value');
              }
            } catch (e) {
              console.error('Ошибка парсинга data-i18n-attr:', e);
            }
          }
        });
      }, 300);
    }

    // Use MutationObserver to detect and translate dynamically added content
    if (window.MutationObserver) {
      const observer = new MutationObserver((mutations) => {
        let needsTranslation = false;
        
        for (const mutation of mutations) {
          if (mutation.type === 'childList' && mutation.addedNodes.length) {
            for (const node of mutation.addedNodes) {
              if (node.nodeType === Node.ELEMENT_NODE) {
                if (node.querySelector('[data-i18n], [data-i18n-attr], [data-i18n-html]') ||
                    node.hasAttribute('data-i18n') ||
                    node.hasAttribute('data-i18n-attr') ||
                    node.hasAttribute('data-i18n-html')) {
                  needsTranslation = true;
                  break;
                }
              }
            }
          }
          if (needsTranslation) break;
        }
        
        if (needsTranslation && window.i18n) {
          window.i18n.updatePageTranslations();
          devLog("RMA Form: MutationObserver обнаружил новое содержимое для перевода");
        }
      });
      
      observer.observe(document.getElementById('devices-container'), {
        childList: true,
        subtree: true
      });
    }
    
    devLog("RMA Form: инициализация завершена");
  }

  /**
   * Create address logic explanation section
   */
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
</ul>`;

      devicesContainer.parentNode.insertBefore(addressInfo, devicesContainer);
      
      // Apply translations using i18next if available
      if (window.i18n && window.i18n.translateDynamicElement) {
        window.i18n.translateDynamicElement(addressInfo);
        devLog("RMA Form: объяснение логики адресов добавлено и переведено");
      } else {
        devLog("RMA Form: объяснение логики адресов добавлено, но i18n не доступен для перевода");
      }
    }
  }

  /**
   * Add a new device entry to the form
   * @returns {HTMLElement} The created device entry
   */
  function addDeviceEntry() {
    deviceCount++;
    const deviceIndex = deviceCount;
    
    devLog(`RMA Form: добавление устройства №${deviceIndex}`);

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
    deviceTitle.style.margin = '0';
    deviceTitle.style.color = '#1e2071';
    
    // ВАЖНОЕ ИСПРАВЛЕНИЕ: Явно устанавливаем текст перед любыми атрибутами
    deviceTitle.textContent = getTranslation('rma:device.title', `Device #${deviceIndex}`).replace('${count}', deviceIndex);

    // Toggle button for alternate shipping address
    const toggleButton = document.createElement('button');
    toggleButton.type = 'button';
    toggleButton.setAttribute('data-i18n', 'rma:device.address_button');
    
    // ВАЖНОЕ ИСПРАВЛЕНИЕ: Явно устанавливаем текст перед любыми атрибутами
    toggleButton.textContent = getTranslation('rma:device.address_button', 'Specify Different Return Address');
    
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
    const alternateAddressSection = createAlternateAddressSection(deviceIndex);
    deviceEntry.appendChild(alternateAddressSection);

    // Create serial number and description section
    const deviceDetailsSection = createDeviceDetailsSection(deviceIndex);
    deviceEntry.appendChild(deviceDetailsSection);

    // Add the complete device entry to the container
    const devicesContainer = document.getElementById('devices-container');
    if (devicesContainer) {
      devicesContainer.appendChild(deviceEntry);

      // Translate the new element
      if (window.i18n && window.i18n.translateDynamicElement) {
        window.i18n.translateDynamicElement(deviceEntry);
        devLog(`RMA Form: устройство №${deviceIndex} добавлено и переведено`);
      } else {
        devLog(`RMA Form: устройство №${deviceIndex} добавлено, но i18n не доступен для перевода`);
      }
    }

    // Set up event listeners for this device entry
    setupDeviceEventListeners(deviceIndex);

    // Display address source info
    updateAddressSourceInfo(deviceIndex);

    return deviceEntry;
  }
  
  /**
   * Create alternate address section
   * @param {number} deviceIndex - The device index
   * @returns {HTMLElement} The created section
   */
  function createAlternateAddressSection(deviceIndex) {
    const alternateAddressSection = document.createElement('div');
    alternateAddressSection.id = `alternate-address-${deviceIndex}`;
    alternateAddressSection.style.display = 'none';
    alternateAddressSection.style.padding = '10px';
    alternateAddressSection.style.border = '1px solid #ddd';
    alternateAddressSection.style.borderRadius = '4px';
    alternateAddressSection.style.marginBottom = '15px';
    alternateAddressSection.style.backgroundColor = '#fff';

    // Add alternate address fields (using i18next for translations if available)
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
<input type="text" id="alt_country_${deviceIndex}" name="alt_country_${deviceIndex}" 
       style="width: 95%; padding: 5px; font-size: 16px; background-color: #eee; margin-top: 5px;">`;

    return alternateAddressSection;
  }
  
  /**
   * Create device details section
   * @param {number} deviceIndex - The device index
   * @returns {HTMLElement} The created section
   */
  function createDeviceDetailsSection(deviceIndex) {
    const deviceDetailsSection = document.createElement('div');

    // Serial number input
    const serialLabel = document.createElement('label');
    serialLabel.htmlFor = `serial${deviceIndex}`;
    serialLabel.setAttribute('data-i18n', 'rma:device.serial_number');
    serialLabel.style.display = 'block';
    serialLabel.style.marginTop = '10px';
    serialLabel.style.fontWeight = 'bold';
    
    // ВАЖНОЕ ИСПРАВЛЕНИЕ: Явно устанавливаем текст перед любыми атрибутами
    serialLabel.textContent = getTranslation('rma:device.serial_number', 'Serial Number:');

    const serialInput = document.createElement('input');
    serialInput.type = 'text';
    serialInput.id = `serial${deviceIndex}`;
    serialInput.name = `serial${deviceIndex}`;
    
    // ВАЖНОЕ ИСПРАВЛЕНИЕ: Устанавливаем явно placeholder перед атрибутом data-i18n-attr
    const serialPlaceholder = getTranslation('rma:device.serial_placeholder', 'Enter the serial number');
    serialInput.placeholder = serialPlaceholder;
    
    // Затем добавляем data-i18n-attr для системы перевода
    serialInput.setAttribute('data-i18n-attr', JSON.stringify({
      "placeholder": "rma:device.serial_placeholder"
    }));
    
    serialInput.style.width = '95%';
    serialInput.style.padding = '5px';
    serialInput.style.fontSize = '18px';
    serialInput.style.backgroundColor = '#eee';
    serialInput.style.marginTop = '5px';

    // Description textarea
    const descLabel = document.createElement('label');
    descLabel.htmlFor = `description${deviceIndex}`;
    descLabel.setAttribute('data-i18n', 'rma:device.issue_description');
    descLabel.style.display = 'block';
    descLabel.style.marginTop = '10px';
    descLabel.style.fontWeight = 'bold';
    
    // ВАЖНОЕ ИСПРАВЛЕНИЕ: Явно устанавливаем текст перед любыми атрибутами
    descLabel.textContent = getTranslation('rma:device.issue_description', 'Issue Description:');

    const descTextarea = document.createElement('textarea');
    descTextarea.id = `description${deviceIndex}`;
    descTextarea.name = `description${deviceIndex}`;
    
    // ВАЖНОЕ ИСПРАВЛЕНИЕ: Устанавливаем явно placeholder перед атрибутом data-i18n-attr
    const descPlaceholder = getTranslation('rma:device.description_placeholder', 'Describe the problem with the device');
    descTextarea.placeholder = descPlaceholder;
    
    // Затем добавляем data-i18n-attr для системы перевода
    descTextarea.setAttribute('data-i18n-attr', JSON.stringify({
      "placeholder": "rma:device.description_placeholder"
    }));
    
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

    return deviceDetailsSection;
  }

  /**
   * Display address source info
   * @param {number} deviceIndex - The device index
   */
  function updateAddressSourceInfo(deviceIndex) {
    const deviceEntries = document.querySelectorAll('.device-entry');
    if (!deviceEntries.length) return;

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

      // ВАЖНО: Обновляем текст в зависимости от состояния
      if (hasAltAddress) {
        // Device has its own address
        toggleBtn.textContent = getTranslation('rma:device.hide_address', 'Hide Return Address');
        toggleBtn.style.backgroundColor = '#e6f7ff';
      } else if (addressSource === 0) {
        // Using billing address
        toggleBtn.textContent = getTranslation('rma:device.using_billing', 'Using Billing Address');
        toggleBtn.style.backgroundColor = '#f0f0f0';
      } else {
        // Using another device's address
        const text = getTranslation('rma:device.using_address', `Using Address from Device #${addressSource}`);
        toggleBtn.textContent = text.replace('${count}', addressSource);
        toggleBtn.style.backgroundColor = '#f0f0f0';
      }
    });
  }

  /**
   * Find nearest address source above
   * @param {number} deviceIndex - The device index
   * @returns {number} The source index (0 for billing)
   */
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

  /**
   * Set up event listeners for device
   * @param {number} deviceIndex - The device index
   */
  function setupDeviceEventListeners(deviceIndex) {
    // Toggle button for alternate address
    const toggleButton = document.querySelector(`.device-entry[data-index="${deviceIndex}"] button`);
    if (toggleButton) {
      toggleButton.addEventListener('click', function() {
        const addressSection = document.getElementById(`alternate-address-${deviceIndex}`);
        if (addressSection) {
          if (addressSection.style.display === 'none') {
            addressSection.style.display = 'block';
            toggleButton.textContent = getTranslation('rma:device.hide_address', 'Hide Return Address');
            toggleButton.style.backgroundColor = '#e6f7ff';

            // Translate the address section if it was just opened
            if (window.i18n && window.i18n.translateDynamicElement) {
              window.i18n.translateDynamicElement(addressSection);
            }
          } else {
            addressSection.style.display = 'none';
            toggleButton.textContent = getTranslation('rma:device.address_button', 'Specify Different Return Address');
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
      descTextarea.addEventListener('focus', function() {
        if (deviceIndex === deviceCount && serialInput.value.trim() !== '') {
          addDeviceEntry();
        }
      });

      // Also check after typing in the serial field
      serialInput.addEventListener('input', function() {
        if (deviceIndex === deviceCount && this.value.trim() !== '') {
          // Check if the description already has focus
          if (document.activeElement !== descTextarea) {
            // If not, we won't add a new entry yet - wait for description focus
          }
        }
      });
    }
  }

  /**
   * Set up form handler
   */
  function setupFormHandler() {
    const rmaForm = document.getElementById('rmaForm');
    if (rmaForm) {
      rmaForm.addEventListener('submit', function(e) {
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
    devLog("RMA Form: ожидание загрузки DOM");
  } else {
    // Set a short timeout to ensure this runs after other scripts
    setTimeout(init, 0);
    devLog("RMA Form: DOM уже загружен, запускаем init через setTimeout");
  }
})();