// File: html/js/rma-form.js
// Improved version that integrates better with the main i18n system

(function () {
  // Current device counter
  let deviceCount = 0;
  let i18nInitialized = false;
  let translationsReady = false;
  let translations = {}; // Translation cache

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
        
        // Set compatibility mode for RMA
        if (window.i18n && typeof window.i18n.setCompatibilityMode === 'function') {
          window.i18n.setCompatibilityMode(true, 'rma');
          console.log("Compatibility mode enabled for RMA");
        }
        
        // Load RMA namespace explicitly
        if (window.i18n && typeof window.i18n.loadNamespaces === 'function') {
          console.log("Loading RMA namespace...");
          window.i18n.loadNamespaces('rma').then(() => {
            console.log("RMA namespace loaded successfully");
            
            // Now it's safe to load translations and add device entry
            if (window.i18n.getCurrentLanguage() !== 'en') {
              loadRmaTranslations().then(() => {
                if (deviceCount === 0) {
                  addDeviceEntry();
                } else {
                  // Update translations for existing elements
                  updateAllTranslations();
                }
              });
            } else {
              // For English, just add the element
              if (deviceCount === 0) {
                addDeviceEntry();
              }
            }
          }).catch(err => {
            console.error("Error loading RMA namespace:", err);
            // Continue anyway
            if (deviceCount === 0) {
              addDeviceEntry();
            }
          });
        } else {
          // Fallback if loadNamespaces is not available
          if (window.i18n.getCurrentLanguage() !== 'en') {
            loadRmaTranslations().then(() => {
              if (deviceCount === 0) {
                addDeviceEntry();
              } else {
                updateAllTranslations();
              }
            });
          } else {
            if (deviceCount === 0) {
              addDeviceEntry();
            }
          }
        }
      });
    } else {
      // i18n is already loaded
      console.log("i18n already loaded, checking if initialized");
      if (window.i18n.isInitialized()) {
        console.log("i18n is already initialized");
        i18nInitialized = true;
        
        // Set compatibility mode for RMA
        if (typeof window.i18n.setCompatibilityMode === 'function') {
          window.i18n.setCompatibilityMode(true, 'rma');
          console.log("Compatibility mode enabled for RMA");
        }
        
        // Load RMA namespace explicitly
        if (typeof window.i18n.loadNamespaces === 'function') {
          console.log("Loading RMA namespace...");
          window.i18n.loadNamespaces('rma').then(() => {
            console.log("RMA namespace loaded successfully");
            
            // Now it's safe to load translations and add device entry
            if (window.i18n.getCurrentLanguage() !== 'en') {
              loadRmaTranslations().then(() => {
                if (deviceCount === 0) {
                  addDeviceEntry();
                } else {
                  updateAllTranslations();
                }
              });
            } else {
              if (deviceCount === 0) {
                addDeviceEntry();
              }
            }
          }).catch(err => {
            console.error("Error loading RMA namespace:", err);
            // Continue anyway
            if (deviceCount === 0) {
              addDeviceEntry();
            }
          });
        } else {
          // Fallback if loadNamespaces is not available
          if (window.i18n.getCurrentLanguage() !== 'en') {
            loadRmaTranslations().then(() => {
              if (deviceCount === 0) {
                addDeviceEntry();
              } else {
                updateAllTranslations();
              }
            });
          } else {
            if (deviceCount === 0) {
              addDeviceEntry();
            }
          }
        }
      } else {
        console.log("i18n is loaded but not yet initialized, waiting for event");
        // i18n is loaded but not initialized, wait for it
        document.addEventListener('i18n:initialized', function() {
          console.log("i18n:initialized event received");
          i18nInitialized = true;
          
          // Set compatibility mode for RMA
          if (window.i18n && typeof window.i18n.setCompatibilityMode === 'function') {
            window.i18n.setCompatibilityMode(true, 'rma');
            console.log("Compatibility mode enabled for RMA");
          }
          
          // Load RMA namespace
          if (window.i18n && typeof window.i18n.loadNamespaces === 'function') {
            console.log("Loading RMA namespace...");
            window.i18n.loadNamespaces('rma').then(() => {
              console.log("RMA namespace loaded successfully");
              
              if (window.i18n.getCurrentLanguage() !== 'en') {
                loadRmaTranslations().then(() => {
                  if (deviceCount === 0) {
                    addDeviceEntry();
                  } else {
                    updateAllTranslations();
                  }
                });
              } else {
                if (deviceCount === 0) {
                  addDeviceEntry();
                }
              }
            }).catch(err => {
              console.error("Error loading RMA namespace:", err);
              // Continue anyway
              if (deviceCount === 0) {
                addDeviceEntry();
              }
            });
          } else {
            // Fallback
            if (window.i18n.getCurrentLanguage() !== 'en') {
              loadRmaTranslations().then(() => {
                if (deviceCount === 0) {
                  addDeviceEntry();
                } else {
                  updateAllTranslations();
                }
              });
            } else {
              if (deviceCount === 0) {
                addDeviceEntry();
              }
            }
          }
        });
      }
    }
  }

  /**
   * Loads and caches translations for the 'rma' namespace without 404 errors
   * @returns {Promise<Object>} - Translation object or empty object if unavailable
   */
  async function loadRmaTranslations() {
    try {
      // If translations are already loaded, just return them
      if (translationsReady) {
        return translations;
      }
      
      const lang = window.i18n.getCurrentLanguage();
      console.log(`Loading RMA translations for ${lang}...`);
      
      // Skip loading for default language (English)
      if (lang === 'en') {
        translations = {};
        translationsReady = true;
        return translations;
      }
      
      // Check global translationFileCache
      if (!window.translationFileCache) {
        // Initialize translation file cache if it doesn't exist
        window.translationFileCache = {
          missingFiles: {},
          cacheExpiry: 5 * 60 * 1000, // 5 minutes
          
          isFileMissing: function(fileKey) {
            if (!this.missingFiles[fileKey]) {
              return false;
            }
            
            // Check if cache has expired
            const timestamp = this.missingFiles[fileKey];
            const now = Date.now();
            
            if (now - timestamp > this.cacheExpiry) {
              delete this.missingFiles[fileKey];
              return false;
            }
            
            return true;
          },
          
          markAsMissing: function(fileKey) {
            this.missingFiles[fileKey] = Date.now();
          },
          
          resetFile: function(fileKey) {
            delete this.missingFiles[fileKey];
          },
          
          resetAll: function() {
            this.missingFiles = {};
          }
        };
      }
      
      const fileKey = `${lang}:rma`;
      
      // Skip file check if we already know it's missing
      if (!window.translationFileCache.isFileMissing(fileKey)) {
        // First use HEAD request to check if file exists without triggering 404 in console
        try {
          // This won't show 404 errors in console
          const checkResponse = await fetch(`/locales/${lang}/rma.json`, {
            method: 'HEAD',
            cache: 'no-cache'
          });
          
          if (checkResponse.ok) {
            // File exists, load it
            const response = await fetch(`/locales/${lang}/rma.json`);
            const data = await response.json();
            console.log(`RMA translations loaded for ${lang}`);
            
            // Flatten and cache translations
            translations = flattenTranslations(data);
            translationsReady = true;
            return translations;
          } else {
            // File doesn't exist - silently mark it as missing
            window.translationFileCache.markAsMissing(fileKey);
            console.log(`No translation file for ${lang}, will use API translation`);
          }
        } catch (error) {
          // Error checking file - mark as missing
          window.translationFileCache.markAsMissing(fileKey);
          console.log(`Error checking for translation file: ${error.message}`);
        }
      } else {
        console.log(`Skipping known missing file for ${lang}`);
      }
      
      // If we get here, we'll rely on the main i18n system
      // Set empty translations object
      translations = {};
      translationsReady = true;
      return translations;
    } catch (error) {
      console.error("Error in loadRmaTranslations:", error);
      
      // Set empty translations as fallback on any error
      translations = {};
      translationsReady = true;
      return translations;
    }
  }

  /**
   * Transforms a nested translation structure into a flat one with dot notation keys
   * For example: { form: { title: "Title" } } => { "form.title": "Title" }
   */
  function flattenTranslations(obj, prefix = '') {
    return Object.keys(obj).reduce((acc, key) => {
      const prefixedKey = prefix ? `${prefix}.${key}` : key;
      
      if (typeof obj[key] === 'object' && obj[key] !== null) {
        Object.assign(acc, flattenTranslations(obj[key], prefixedKey));
      } else {
        acc[prefixedKey] = obj[key];
      }
      
      return acc;
    }, {});
  }

  /**
   * Gets translation with appropriate fallback
   * @param {string} key - Translation key
   * @param {Object} options - Options (count, etc.)
   * @param {string} fallback - Fallback text
   * @returns {string} - Translated text or fallback
   */
  function getTranslation(key, options = {}, fallback = '') {
    // Use main i18n system if available
    if (window.i18n && typeof window.i18n.t === 'function') {
      // Always prefix with 'rma:' if not already present
      const adjustedKey = key.includes(':') ? key : `rma:${key}`;
      
      // Special case handling for device references
      if (key === 'device.using_address' && options.count !== undefined) {
        // Use i18n.t with the right options
        return window.i18n.t(adjustedKey, {...options, defaultValue: fallback || `Using Address from Device #${options.count}`});
      }
      
      // Use i18n.t for translation
      return window.i18n.t(adjustedKey, {...options, defaultValue: fallback || key.split('.').pop()});
    }
    
    // Fallback: our local implementation
    if (!i18nInitialized || !translationsReady || window.i18n?.getCurrentLanguage() === 'en') {
      // For device references, format the fallback with the device number
      if (key === 'device.using_address' && options.count !== undefined) {
        return fallback || `Using Address from Device #${options.count}`;
      }
      
      // Use provided fallback or extract from key (e.g., "device.title" -> "title")
      return fallback || key.split('.').pop(); 
    }
    
    try {
      // Remove 'rma:' prefix if it exists
      const cleanKey = key.includes(':') ? key.split(':')[1] : key;
      
      // Find translation in our cache
      if (translations[cleanKey]) {
        let result = translations[cleanKey];
        
        // Special case for device titles
        if (cleanKey === 'device.title') {
          // For device title, just return the word "Gerät" (or equivalent)
          return result;
        }
        else if (cleanKey === 'device.using_address' && options.count !== undefined) {
          // For "Using address from Device #X" messages
          return result.replace(/\{\{count\}\}/g, options.count);
        }
        // Normal placeholder substitution for other cases
        else if (options.count !== undefined && result.includes('{{count}}')) {
          result = result.replace(/\{\{count\}\}/g, options.count);
        }
        
        return result;
      }
      
      // Not found in local cache - use main i18n system if available
      if (window.i18n && typeof window.i18n.t === 'function') {
        // Always prefix with 'rma:' if not already present
        const adjustedKey = key.includes(':') ? key : `rma:${key}`;
        return window.i18n.t(adjustedKey, {...options, defaultValue: fallback || key.split('.').pop()});
      }
      
      // Final fallback
      if (key === 'device.using_address' && options.count !== undefined) {
        return fallback || `Using Address from Device #${options.count}`;
      }
      
      return fallback || key.split('.').pop();
    } catch (e) {
      console.error(`Error getting translation for ${key}:`, e);
      
      // Safety fallback for device references even if error occurs
      if (key === 'device.using_address' && options.count !== undefined) {
        return `Using Address from Device #${options.count}`;
      }
      
      return fallback || key.split('.').pop();
    }
  }

  /**
   * Updates translations for all elements in the form
   */
  function updateAllTranslations() {
    // Update translations for address logic info
    const addressInfo = document.querySelector('.address-logic-info');
    if (addressInfo) {
      manuallyTranslateElement(addressInfo);
    }
    
    // Update translations for all device entries
    const deviceEntries = document.querySelectorAll('.device-entry');
    deviceEntries.forEach(entry => {
      manuallyTranslateElement(entry);
    });
    
    // Update status texts on buttons
    for (let i = 1; i <= deviceCount; i++) {
      updateAddressSourceInfo(i);
    }
  }

  /**
   * Применяет переводы к элементу и его дочерним элементам
   * @param {HTMLElement} element - Контейнер с элементами для перевода
   */
  function manuallyTranslateElement(element) {
    // Используем общий метод, если доступен
    if (window.i18n && typeof window.i18n.translateElement === 'function') {
      // Включаем параметр namespace для RMA
      window.i18n.translateElement(element, { namespace: 'rma' });
      return;
    }
    
    // Если общий метод недоступен, используем оригинальную реализацию
    console.warn('i18n.translateElement is not available, using original implementation');
    
    // Пропускаем если используем язык по умолчанию
    if (!window.i18n || window.i18n.getCurrentLanguage() === 'en') {
      return;
    }
    
    // Обрабатываем элементы с data-i18n
    const elementsWithI18n = [element, ...element.querySelectorAll('[data-i18n]')];
    elementsWithI18n.forEach(el => {
      if (el.hasAttribute('data-i18n')) {
        const key = el.getAttribute('data-i18n');
        
        // Извлекаем опции из атрибута data-i18n-options
        let options = {};
        try {
          const optionsAttr = el.getAttribute('data-i18n-options');
          if (optionsAttr) {
            options = JSON.parse(optionsAttr);
          }
        } catch (parseError) {
          console.error(`Error parsing data-i18n-options for ${key}:`, parseError);
        }
        
        // Получаем перевод
        const translation = getTranslation(key.includes(':') ? key : 'rma:' + key, options);
        if (translation && translation !== key) {
          el.textContent = translation;
          
          // ВАЖНОЕ ИСПРАВЛЕНИЕ: удаляем атрибут data-i18n после перевода!
          el.removeAttribute('data-i18n');
          
          // Также удаляем data-i18n-options если он есть
          if (el.hasAttribute('data-i18n-options')) {
            el.removeAttribute('data-i18n-options');
          }
          
          console.log(`[RMA] Removed data-i18n after translation for "${key}"`);
        }
      }
    });
    
    // Обрабатываем атрибутные переводы (data-i18n-attr)
    const elementsWithAttrI18n = [element, ...element.querySelectorAll('[data-i18n-attr]')];
    elementsWithAttrI18n.forEach(el => {
      if (el.hasAttribute('data-i18n-attr')) {
        try {
          const attrsMap = JSON.parse(el.getAttribute('data-i18n-attr'));
          
          // Извлекаем опции для этого элемента
          let options = {};
          const optionsAttr = el.getAttribute('data-i18n-options');
          if (optionsAttr) {
            try {
              options = JSON.parse(optionsAttr);
            } catch (parseError) {
              console.error(`Error parsing data-i18n-options:`, parseError);
            }
          }
          
          // Счетчик для отслеживания успешных переводов
          let successfulTranslations = 0;
          const totalAttributes = Object.keys(attrsMap).length;
          
          // Обрабатываем каждый атрибут
          for (const [attr, key] of Object.entries(attrsMap)) {
            const translation = getTranslation(key.includes(':') ? key : 'rma:' + key, options);
            if (translation && translation !== key) {
              el.setAttribute(attr, translation);
              successfulTranslations++;
            }
          }
          
          // ВАЖНОЕ ИСПРАВЛЕНИЕ: удаляем data-i18n-attr после перевода всех атрибутов!
          if (successfulTranslations === totalAttributes) {
            el.removeAttribute('data-i18n-attr');
            
            // Также удаляем data-i18n-options если он есть
            if (el.hasAttribute('data-i18n-options')) {
              el.removeAttribute('data-i18n-options');
            }
            
            console.log(`[RMA] Removed data-i18n-attr after translating all ${totalAttributes} attributes`);
          }
        } catch (e) {
          console.error('Error parsing data-i18n-attr:', e);
        }
      }
    });
    
    // Обрабатываем HTML переводы (data-i18n-html)
    const elementsWithHtmlI18n = [element, ...element.querySelectorAll('[data-i18n-html]')];
    elementsWithHtmlI18n.forEach(el => {
      if (el.hasAttribute('data-i18n-html')) {
        const key = el.getAttribute('data-i18n-html');
        
        // Извлекаем опции
        let options = {};
        const optionsAttr = el.getAttribute('data-i18n-options');
        if (optionsAttr) {
          try {
            options = JSON.parse(optionsAttr);
          } catch (parseError) {
            console.error(`Error parsing data-i18n-options for HTML ${key}:`, parseError);
          }
        }
        
        const translation = getTranslation(key.includes(':') ? key : 'rma:' + key, options);
        if (translation && translation !== key) {
          el.innerHTML = translation;
          
          // ВАЖНОЕ ИСПРАВЛЕНИЕ: удаляем data-i18n-html после перевода!
          el.removeAttribute('data-i18n-html');
          
          // Также удаляем data-i18n-options если он есть
          if (el.hasAttribute('data-i18n-options')) {
            el.removeAttribute('data-i18n-options');
          }
          
          console.log(`[RMA] Removed data-i18n-html after translation for "${key}"`);
        }
      }
    });
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
    langTitle.textContent = 'Language / Sprache: ';
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
   * Adds an explanation of the address logic
   */
  function addAddressLogicExplanation() {
    const devicesContainer = document.getElementById('devices-container');
    if (!devicesContainer) return;
    
    const addressInfo = document.createElement('div');
    addressInfo.className = 'address-logic-info';
    addressInfo.style.padding = '10px';
    addressInfo.style.marginBottom = '20px';
    addressInfo.style.backgroundColor = '#e6f7ff';
    addressInfo.style.border = '1px solid #91d5ff';
    addressInfo.style.borderRadius = '4px';
    addressInfo.style.fontSize = '14px';
    addressInfo.className = 'text2black';

    addressInfo.innerHTML = `
      <p class="text2blue" data-i18n="address_logic.title"><strong>Device Return Information:</strong></p>
      <ul style="margin-top: 5px; padding-left: 20px;">
        <li data-i18n="address_logic.info.0">By default, all devices are shipped to the address specified in the "Billing Information" section.</li>
        <li data-i18n="address_logic.info.1">You can specify an alternative return address for any device.</li>
        <li data-i18n="address_logic.info.2">If you haven't specified an address for a device, the nearest address above in the list will be used.</li>
      </ul>
    `;

    devicesContainer.parentNode.insertBefore(addressInfo, devicesContainer);
    
    // Apply translations manually
    if (window.i18n && window.i18n.isInitialized() && window.i18n.getCurrentLanguage() !== 'en') {
      manuallyTranslateElement(addressInfo);
    }
  }

  /**
   * Add a new device entry to the RMA form with proper translation support
   * @returns {HTMLElement} The created device entry element
   */
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

    // Create title with separate span for the number
    const deviceTitle = document.createElement('h4');
    deviceTitle.style.margin = '0';
    deviceTitle.style.color = '#1e2071';
    
    // Create a span for the translatable part
    const titleText = document.createElement('span');
    titleText.className = 'text2blue';
    titleText.setAttribute('data-i18n', 'device.title');
    titleText.textContent = 'Device';
    
    // Create a span for the number (non-translatable)
    const titleNumber = document.createElement('span');
    titleNumber.className = 'text2blue';
    titleNumber.textContent = ` #${deviceIndex}`;
    
    // Append both parts to the title
    deviceTitle.appendChild(titleText);
    deviceTitle.appendChild(titleNumber);

    // Toggle button for alternate shipping address
    const toggleButton = document.createElement('button');
    toggleButton.type = 'button';
    toggleButton.setAttribute('data-i18n', 'device.address_button');
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
    alternateAddressSection.className = 'text2black';

    // Add alternate address fields
    alternateAddressSection.innerHTML = `
      <h4 style="margin-top: 0; color: #1e2071;" data-i18n="device.alternate_address">Alternate Return Address</h4>
      <p style="font-style: italic; margin-bottom: 10px;" data-i18n="device.address_info">Specify a different address for returning this device.</p>
      
      <label for="alt_company_${deviceIndex}" style="display: block; margin-top: 10px;"><b data-i18n="form.company_name">Company Name:</b></label>
      <input type="text" id="alt_company_${deviceIndex}" name="alt_company_${deviceIndex}" 
             style="width: 95%; padding: 5px; font-size: 16px; background-color: #eee; margin-top: 5px;">

      <label for="alt_person_${deviceIndex}" style="display: block; margin-top: 10px;" data-i18n="form.contact_person">Contact Person:</label>
      <input type="text" id="alt_person_${deviceIndex}" name="alt_person_${deviceIndex}" 
             style="width: 95%; padding: 5px; font-size: 16px; background-color: #eee; margin-top: 5px;">

      <label for="alt_street_${deviceIndex}" style="display: block; margin-top: 10px;"><b data-i18n="form.street">Street and House Number:</b></label>
      <input type="text" id="alt_street_${deviceIndex}" name="alt_street_${deviceIndex}" 
             style="width: 95%; padding: 5px; font-size: 16px; background-color: #eee; margin-top: 5px;">

      <label for="alt_addressLine2_${deviceIndex}" style="display: block; margin-top: 10px;" data-i18n="form.additional_address">Additional Address Line:</label>
      <input type="text" id="alt_addressLine2_${deviceIndex}" name="alt_addressLine2_${deviceIndex}" 
             style="width: 95%; padding: 5px; font-size: 16px; background-color: #eee; margin-top: 5px;">

      <label for="alt_postalCode_${deviceIndex}" style="display: block; margin-top: 10px;"><b data-i18n="form.postal_code">Postal Code:</b></label>
      <input type="text" id="alt_postalCode_${deviceIndex}" name="alt_postalCode_${deviceIndex}" 
             style="width: 95%; padding: 5px; font-size: 16px; background-color: #eee; margin-top: 5px;">

      <label for="alt_city_${deviceIndex}" style="display: block; margin-top: 10px;"><b data-i18n="form.city">City:</b></label>
      <input type="text" id="alt_city_${deviceIndex}" name="alt_city_${deviceIndex}" 
             style="width: 95%; padding: 5px; font-size: 16px; background-color: #eee; margin-top: 5px;">

      <label for="alt_country_${deviceIndex}" style="display: block; margin-top: 10px;"><b data-i18n="form.country">Country:</b></label>
      <input type="text" id="alt_country_${deviceIndex}" name="alt_country_${deviceIndex}" 
             style="width: 95%; padding: 5px; font-size: 16px; background-color: #eee; margin-top: 5px;">
    `;

    deviceEntry.appendChild(alternateAddressSection);

    // Create serial number and description section
    const deviceDetailsSection = document.createElement('div');

    // Serial number input
    const serialLabel = document.createElement('label');
    serialLabel.htmlFor = `serial${deviceIndex}`;
    serialLabel.className = 'text2black';
    serialLabel.setAttribute('data-i18n', 'device.serial_number');
    serialLabel.textContent = 'Serial Number:';
    serialLabel.style.display = 'block';
    serialLabel.style.marginTop = '10px';
    serialLabel.style.fontWeight = 'bold';

    const serialInput = document.createElement('input');
    serialInput.type = 'text';
    serialInput.id = `serial${deviceIndex}`;
    serialInput.name = `serial${deviceIndex}`;
    serialInput.setAttribute('data-i18n-attr', JSON.stringify({"placeholder": "device.serial_placeholder"}));
    serialInput.style.width = '95%';
    serialInput.style.padding = '5px';
    serialInput.style.fontSize = '18px';
    serialInput.style.backgroundColor = '#eee';
    serialInput.style.marginTop = '5px';

    // Description textarea
    const descLabel = document.createElement('label');
    descLabel.className = 'text2black';
    descLabel.htmlFor = `description${deviceIndex}`;
    descLabel.setAttribute('data-i18n', 'device.issue_description');
    descLabel.textContent = 'Issue Description:';
    descLabel.style.display = 'block';
    descLabel.style.marginTop = '10px';
    descLabel.style.fontWeight = 'bold';

    const descTextarea = document.createElement('textarea');
    descTextarea.id = `description${deviceIndex}`;
    descTextarea.name = `description${deviceIndex}`;
    descTextarea.setAttribute('data-i18n-attr', JSON.stringify({"placeholder": "device.description_placeholder"}));
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
    const devicesContainer = document.getElementById('devices-container');
    if (devicesContainer) {
      devicesContainer.appendChild(deviceEntry);

      // Translate the new element
      if (window.i18n && window.i18n.isInitialized() && window.i18n.getCurrentLanguage() !== 'en') {
        manuallyTranslateElement(deviceEntry);
      }
    }

    // Set up event listeners for this device entry
    setupDeviceEventListeners(deviceIndex);

    // Display address source info
    updateAddressSourceInfo(deviceIndex);

    return deviceEntry;
  }

  /**
   * Updates the display of address source information
   * @param {number} deviceIndex - Index of the device to update
   * @param {boolean} isRetry - Whether this is a retry attempt
   */
  function updateAddressSourceInfo(deviceIndex, isRetry = false) {
    // If translations aren't ready and this isn't a retry attempt
    if (!translationsReady && window.i18n && window.i18n.getCurrentLanguage() !== 'en' && !isRetry) {
      // Load translations once, then retry
      loadRmaTranslations().then(() => {
        // Mark isRetry=true to prevent infinite recursion
        updateAddressSourceInfo(deviceIndex, true);
      }).catch(() => {
        // Continue with English fallbacks
        updateAddressSourceInfoEnglish(deviceIndex);
      });
      return;
    }
    
    const deviceEntries = document.querySelectorAll('.device-entry');

    // Loop through all devices
    deviceEntries.forEach(entry => {
      const entryIndex = parseInt(entry.dataset.index, 10);

      // Skip devices with index lower than current
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
        toggleBtn.textContent = getTranslation('device.hide_address', {}, 'Hide Return Address');
        toggleBtn.style.backgroundColor = '#e6f7ff';
      } else if (addressSource === 0) {
        // Using billing address
        toggleBtn.textContent = getTranslation('device.using_billing', {}, 'Using Billing Address');
        toggleBtn.style.backgroundColor = '#f0f0f0';
      } else {
        // Using another device's address
        const addressText = getTranslation('device.using_address', {count: addressSource}, 'Using Address from Device #' + addressSource);
        toggleBtn.textContent = addressText;
        toggleBtn.style.backgroundColor = '#f0f0f0';
      }
    });
  }

  /**
   * Fallback function that uses English strings when translations aren't available
   * @param {number} deviceIndex - Index of the device to update
   */
  function updateAddressSourceInfoEnglish(deviceIndex) {
    const deviceEntries = document.querySelectorAll('.device-entry');

    // Loop through all devices
    deviceEntries.forEach(entry => {
      const entryIndex = parseInt(entry.dataset.index, 10);

      // Skip devices with index lower than current
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
        toggleBtn.textContent = 'Hide Return Address';
        toggleBtn.style.backgroundColor = '#e6f7ff';
      } else if (addressSource === 0) {
        // Using billing address
        toggleBtn.textContent = 'Using Billing Address';
        toggleBtn.style.backgroundColor = '#f0f0f0';
      } else {
        // Using another device's address
        toggleBtn.textContent = `Using Address from Device #${addressSource}`;
        toggleBtn.style.backgroundColor = '#f0f0f0';
      }
    });
  }

  /**
   * Find nearest address source above
   * @param {number} deviceIndex - Device index to check
   * @returns {number} - Index of nearest address source, 0 for billing address
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
   * @param {number} deviceIndex - Device index to set up
   */
  function setupDeviceEventListeners(deviceIndex) {
    // Toggle button for alternate address
    const toggleButton = document.querySelector(`.device-entry[data-index="${deviceIndex}"] button`);
    if (toggleButton) {
      toggleButton.addEventListener('click', function () {
        const addressSection = document.getElementById(`alternate-address-${deviceIndex}`);
        if (addressSection) {
          if (addressSection.style.display === 'none') {
            addressSection.style.display = 'block';
            toggleButton.textContent = getTranslation('device.hide_address', {}, 'Hide Return Address');
            toggleButton.style.backgroundColor = '#e6f7ff';

            // Translate the address section if it was just opened
            if (window.i18n && window.i18n.isInitialized() && window.i18n.getCurrentLanguage() !== 'en') {
              manuallyTranslateElement(addressSection);
            }
          } else {
            addressSection.style.display = 'none';
            toggleButton.textContent = getTranslation('device.address_button', {}, 'Specify Different Return Address');
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

  /**
   * Set up form handler
   */
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