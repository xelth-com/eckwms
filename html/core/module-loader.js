// html/core/module-loader.js

/**
 * Core module loader for dynamic component loading.
 * Handles loading HTML/JS modules and inserting them into specified containers.
 * UPDATED FOR ERROR RESILIENCE.
 */

/**
 * Load a module and insert its content into a container.
 * Logs errors but does not reject the promise on failure to load module JS or its CSS.
 * Rejects only if the container itself is not found.
 * @param {string} name - Module name for logging purposes.
 * @param {string} path - Path to the module's JavaScript file (e.g., '/my-module/my-module.js').
 * @param {string} containerId - ID of the HTML element where the module's content should be rendered.
 * @returns {Promise<object|null>} - Promise that resolves with the loaded module object if successful,
 *                                  or null if the module JS/CSS/template fails to load or init errors occur.
 *                                  Rejects only if the specified container element is not found in the DOM.
 */
export async function loadModule(name, path, containerId) {
  console.log(`[ModuleLoader] Loading module: ${name} from ${path} into #${containerId}`);

  const container = document.getElementById(containerId);
  if (!container) {
      const errorMsg = `[ModuleLoader] CRITICAL: Container element #${containerId} not found for module "${name}". Cannot load module.`;
      console.error(errorMsg);
      // Reject promise only for this critical error.
      return Promise.reject(new Error(errorMsg));
  }

  try {
      // Attempt to dynamically import the module's JavaScript file.
      const module = await import(path);
      console.log(`[ModuleLoader] Module "${name}" imported successfully.`);

      // Check if the module has an 'init' function and execute it.
      if (typeof module.init === 'function') {
          try {
              // The init function is expected to load CSS/HTML and set up the module.
              await module.init(container);
              console.log(`[ModuleLoader] Module "${name}" initialized.`);
          } catch (initError) {
              console.error(`[ModuleLoader] Error during init() for module "${name}":`, initError);
              // Optionally display an error message in the container for the user.
              container.innerHTML = `<p style="color: red; padding: 10px;">Error initializing module: ${name}.</p>`;
              return null; // Resolve with null to indicate module init failure.
          }
      } else {
          console.warn(`[ModuleLoader] Module "${name}" has no init function.`);
          // If the container still shows a loading indicator, clear it.
          if (container.innerHTML.includes('loading-indicator')) {
              container.innerHTML = ''; // Clear loading indicator if init doesn't provide content
          }
      }

      // Check for and execute a 'postInit' function after a brief delay.
      if (typeof module.postInit === 'function') {
          // Use setTimeout to ensure the DOM has potentially updated from init().
          setTimeout(() => {
              try {
                  module.postInit(container);
                  console.log(`[ModuleLoader] Module "${name}" postInit executed.`);
              } catch (postInitError) {
                  console.error(`[ModuleLoader] Error during postInit() for module "${name}":`, postInitError);
                  // postInit errors are logged but generally don't break the flow here.
              }
          }, 0);
      }

      // Resolve the promise with the loaded module object.
      return module;

  } catch (importError) {
      // Handle errors during the dynamic import (e.g., JS file not found, syntax errors).
      console.error(`[ModuleLoader] Failed to import module "${name}" from ${path}:`, importError);
      // Display an error message within the container.
      container.innerHTML = `<p style="color: red; padding: 10px;">Error loading module: ${name}.</p>`;
      // Resolve with null to indicate failure but allow other operations to continue.
      return null;
  }
}

/**
* Load HTML content from a template file.
* Logs errors but resolves with an empty string on failure to ensure robustness.
* @param {string} path - Path to the HTML template file (e.g., '/my-module/my-module.template.html').
* @returns {Promise<string>} - Promise that resolves with the HTML content as a string, or an empty string if loading fails.
*/
export async function loadTemplate(path) {
  try {
      console.log(`[ModuleLoader] Loading template: ${path}`);
      const response = await fetch(path);
      if (!response.ok) {
          // Log specific HTTP error but don't throw, resolve with empty string.
          console.error(`[ModuleLoader] Failed to load template: ${response.status} ${response.statusText} for path ${path}`);
          return ''; // Return empty string on HTTP errors (like 404).
      }
      const html = await response.text();
      console.log(`[ModuleLoader] Template loaded successfully: ${path}`);
      return html;
  } catch (error) {
      // Log network or other errors during fetch.
      console.error(`[ModuleLoader] Network error loading template from ${path}:`, error);
      return ''; // Return empty string on general errors.
  }
}

/**
* Load a CSS file dynamically.
* Logs errors but resolves the promise even if the CSS fails to load, preventing blockage.
* @param {string} path - Path to the CSS file (e.g., '/my-module/my-module.css').
* @returns {Promise<void>} - Promise that resolves when the loading attempt is complete (whether successful or not).
*/
export function loadCSS(path) {
  return new Promise((resolve) => { // Changed from reject to always resolve
      // Check if already loaded
      if (document.querySelector(`link[rel="stylesheet"][href="${path}"]`)) {
          console.log(`[ModuleLoader] CSS already loaded: ${path}`);
          resolve();
          return;
      }

      console.log(`[ModuleLoader] Loading CSS: ${path}`);
      const link = document.createElement('link');
      link.rel = 'stylesheet';
      link.href = path;

      // Resolve the promise once the CSS is successfully loaded.
      link.onload = () => {
          console.log(`[ModuleLoader] CSS loaded successfully: ${path}`);
          resolve(); // Resolve on success
      };

      // Log errors but still resolve the promise to avoid blocking other operations.
      link.onerror = (error) => {
          // Log the error but still resolve the promise
          console.error(`[ModuleLoader] Failed to load CSS: ${path}`, error);
          resolve(); // Resolve even on error
      };

      // Append the link element to the document's head to initiate loading.
      document.head.appendChild(link);
  });
}

/**
* Lazy load a module only when a specific condition is met.
* @param {string} name - Module name for logging.
* @param {string} path - Path to the module JS file.
* @param {string} containerId - ID of the container to insert the module into.
* @param {Function} condition - A function that returns true if the module should be loaded.
* @returns {Promise<object|null>} - Promise resolving with the module or null, based on loadModule's result, or null if condition is false.
*/
export function lazyLoadModule(name, path, containerId, condition) {
  // Check the condition function before attempting to load.
  if (typeof condition === 'function' && !condition()) {
      console.log(`[ModuleLoader] Condition not met for lazy loading module "${name}". Skipping.`);
      return Promise.resolve(null); // Condition not met, resolve with null.
  }
  // Condition met or not provided, proceed to load the module.
  return loadModule(name, path, containerId);
}