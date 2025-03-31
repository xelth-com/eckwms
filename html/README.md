# Modular HTML Structure - Implementation Guide

This document outlines the modular structure created for the website. This approach separates the HTML into reusable components that can be individually loaded and utilized across multiple pages.

## Directory Structure

```
/
  ├── core/
  │   ├── module-loader.js         # Core module loading functionality
  │   └── global-utils.js          # Common utility functions
  │
  ├── header/
  │   ├── header.js                # Header module functionality
  │   ├── header.css               # Header-specific styles
  │   └── header.template.html     # Header HTML template
  │
  ├── footer/
  │   ├── footer.js                # Footer module functionality
  │   ├── footer.css               # Footer-specific styles
  │   └── footer.template.html     # Footer HTML template
  │
  ├── content/
  │   ├── home-content.js          # Home page content functionality
  │   ├── home-content.css         # Home page content styles
  │   └── home-content.template.html # Home page content template
  │   └── ... other content modules
  │
  ├── navigation/
  │   ├── side-menu.js             # Side menu functionality
  │   ├── side-menu.css            # Side menu styles
  │   └── side-menu.template.html  # Side menu template
  │
  ├── utils/
  │   ├── fetch-utils.js           # AJAX functionality
  │   └── image-utils.js           # Image-related utilities
  │
  ├── i18n/
  │   ├── language-selector.js     # Language selection functionality
  │   └── i18n.css                 # Translation-related styles
  │
  ├── svg/
  │   ├── svg-defs.js              # SVG definitions functionality
  │   └── svg-defs.template.html   # SVG definitions template
  │
  └── common/
      └── global.css               # Global styles
```

## How It Works

1. The base HTML file (`index.html`) contains only the basic structure with container elements
2. The module loader (`module-loader.js`) dynamically loads each module into its respective container
3. Each module consists of:
   - A JavaScript file that handles functionality
   - A CSS file for styles
   - An HTML template file for structure
4. Modules are initialized in a specific order to ensure dependencies are met

## Adding a New Page

To create a new page using this modular structure:

1. Create a new HTML file based on the basic structure in `index.html`
2. Change the content module to load the appropriate content for that page:
   ```javascript
   await loadModule('main-content', '/modules/content/new-page-content.js', 'main-content-container');
   ```
3. Create the new content module with its JS, CSS, and HTML template files

## Creating a New Module

To create a new module:

1. Create a directory in `/modules/` for your module (or use an existing category)
2. Create three files:
   - `your-module.js` - Module functionality
   - `your-module.css` - Module styles
   - `your-module.template.html` - Module HTML template
3. Use this basic structure for the JS file:
   ```javascript
   import { loadCSS, loadTemplate } from '/modules/core/module-loader.js';

   export async function init(container) {
     // Load CSS
     await loadCSS('/modules/your-directory/your-module.css');
     
     // Load HTML template
     const html = await loadTemplate('/modules/your-directory/your-module.template.html');
     container.innerHTML = html;
     
     // Initialize functionality
     initEventListeners();
   }

   function initEventListeners() {
     // Add event listeners and functionality
   }

   export function postInit() {
     // Any tasks to run after DOM is updated
   }
   ```

## Benefits of This Approach

1. **Reusability**: Components can be easily reused across multiple pages
2. **Maintainability**: Each component is isolated, making it easier to maintain
3. **Performance**: Only the necessary components are loaded for each page
4. **Scalability**: New components can be added without modifying existing code
5. **Collaboration**: Different team members can work on different components

## Loading Order Considerations

Certain components depend on others (e.g., the header depends on SVG definitions). The recommended loading order is:

1. SVG Definitions (contains graphics used by other components)
2. Header (contains language selection and navigation)
3. Main Content (specific to each page)
4. Side Menu (mobile navigation)
5. Footer (appears at the bottom of every page)

## Translation Integration

The modular structure works with the i18n system:

1. Translation data attributes (`data-i18n`, `data-i18n-attr`, etc.) are included in HTML templates
2. After loading a module, the `i18n.updatePageTranslations()` function is called to apply translations
3. Language selection is handled in the header module

## Notes on SVG Usage

SVG definitions are loaded first because they provide icons and filters used by other components. The SVG module:

1. Loads all SVG definitions into a container
2. Provides background elements for the site
3. Sets random seeds for SVG effects
4. Handles browser-specific adjustments for SVG filters
