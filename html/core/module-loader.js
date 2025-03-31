/**
 * Core module loader for dynamic component loading
 * Handles loading HTML/JS modules and inserting them into specified containers
 */

/**
 * Load a module and insert its content into a container
 * @param {string} name - Module name for logging
 * @param {string} path - Path to the module JS file
 * @param {string} containerId - ID of container to insert the module
 * @returns {Promise} - Promise that resolves when module is loaded
 */
export async function loadModule(name, path, containerId) {
  console.log(`Loading module: ${name}`);
  
  try {
    // Load the module
    const module = await import(path);
    
    // Get the container
    const container = document.getElementById(containerId);
    if (!container) {
      console.error(`Container not found: ${containerId}`);
      return Promise.reject(`Container not found: ${containerId}`);
    }
    
    // Initialize the module
    if (typeof module.init === 'function') {
      await module.init(container);
    } else {
      console.warn(`Module ${name} has no init function`);
    }
    
    // Run post-initialization if needed
    if (typeof module.postInit === 'function') {
      // Use setTimeout to ensure DOM has updated
      setTimeout(() => module.postInit(container), 0);
    }
    
    return Promise.resolve(module);
  } catch (error) {
    console.error(`Failed to load module ${name}:`, error);
    return Promise.reject(error);
  }
}

/**
 * Load HTML content from a template
 * @param {string} path - Path to the HTML template file
 * @returns {Promise<string>} - Promise that resolves with the HTML content
 */
export async function loadTemplate(path) {
  try {
    const response = await fetch(path);
    if (!response.ok) {
      throw new Error(`Failed to load template: ${response.status} ${response.statusText}`);
    }
    return await response.text();
  } catch (error) {
    console.error(`Error loading template from ${path}:`, error);
    return Promise.reject(error);
  }
}

/**
 * Lazy load a module only when needed
 * @param {string} name - Module name for logging
 * @param {string} path - Path to the module JS file
 * @param {string} containerId - ID of container to insert the module
 * @param {Function} condition - Function that returns true when module should be loaded
 * @returns {Promise} - Promise that resolves when module is loaded (or immediately if condition is false)
 */
export function lazyLoadModule(name, path, containerId, condition) {
  if (typeof condition === 'function' && !condition()) {
    return Promise.resolve(null);
  }
  return loadModule(name, path, containerId);
}

/**
 * Load a CSS file dynamically
 * @param {string} path - Path to the CSS file
 * @returns {Promise} - Promise that resolves when CSS is loaded
 */
export function loadCSS(path) {
  return new Promise((resolve, reject) => {
    // Check if already loaded
    if (document.querySelector(`link[href="${path}"]`)) {
      resolve();
      return;
    }
    
    const link = document.createElement('link');
    link.rel = 'stylesheet';
    link.href = path;
    
    link.onload = () => resolve();
    link.onerror = () => reject(new Error(`Failed to load CSS: ${path}`));
    
    document.head.appendChild(link);
  });
}
