// html/utils/image-utils.js
/**
 * Image Utilities Module
 * Handles image-related operations including fullscreen viewing,
 * SVG filter adjustments, and visual effects for pins and stickers.
 */

/**
 * Initializes the module
 * @param {HTMLElement} container - Container element (not used in this module)
 */
export async function init(container) {
  console.log("Initializing image utilities module");
  
  // Store reference to device pixel ratio
  window.devicePixelRatioWas = window.devicePixelRatio;
  
  // Export functions to global scope for access from other modules
  window.fsPic = fsPic;
  window.picFromSet = picFromSet;
  window.updatePixelRatio = updatePixelRatio;
  window.applyRandomEffects = applyRandomEffects;
  
  // Update SVG filters based on pixel ratio
  updatePixelRatio();
}

/**
 * Post-initialization tasks - called after DOM is updated
 */
export function postInit() {
  console.log("Post-initializing image utilities module");
  
  // Apply random effects to all stickers and pins
  setTimeout(() => {
    console.log("Applying random effects after delay to ensure DOM is ready");
    applyRandomEffects();
  }, 100);
  
  // Set up observer for dynamically added content
  initDynamicContentObserver();
  
  // Set up listener for device pixel ratio changes
  setupPixelRatioChangeListener();
}

/**
 * Shows an image in fullscreen mode with zoom capabilities
 * @param {string} name - Path to the image
 */
export function fsPic(name) {
  const el = document.getElementById("fsPic");
  if (!el) {
    console.error("Fullscreen container not found");
    return;
  }
  
  const img = new Image();
  
  img.onload = function() {
    el.style.background = `linear-gradient(#b69c6850, #c6b89b50, #b69c6850),
      linear-gradient(90deg,#b69c68, #c6b89b, #b69c68)`;
    el.style.overflow = 'hidden';
    this.style.cursor = 'zoom-in';

    // Scale image properly based on viewport
    if (this.width / window.innerWidth > this.height / window.innerHeight) {
      this.style.marginTop = (window.innerHeight - this.height * window.innerWidth / this.width) / 2 + 'px';
      this.width = window.innerWidth;
    } else {
      this.style.marginLeft = (window.innerWidth - this.width * window.innerHeight / this.height) / 2 + 'px';
      this.height = window.innerHeight;
    }
    
    // Create event handlers outside for removal capability
    let mouseMoveHandler;
    const clickHandler = (event) => {
      if (this.style.cursor === 'zoom-in') {
        this.style.cursor = 'zoom-out';
        el.style.overflow = 'scroll';
        this.width = this.naturalWidth;
        this.height = this.naturalHeight;
        this.style.marginTop = (window.innerHeight - this.height) > 0 ? (window.innerHeight - this.height) / 2 + 'px' : 0;
        this.style.marginLeft = (window.innerWidth - this.width) > 0 ? (window.innerWidth - this.width) / 2 + 'px' : 0;
        
        // Calculate scroll position
        const tx = 2 * (this.width - window.innerWidth) / window.innerWidth;
        const ty = 2 * (this.height - window.innerHeight) / window.innerHeight;
        
        // Initial scroll position
        el.scroll((event.clientX - window.innerWidth / 4) * tx, (event.clientY - window.innerHeight / 4) * ty);
        
        // Add mousemove event for scrolling
        mouseMoveHandler = (moveEvent) => {
          el.scroll((moveEvent.clientX - window.innerWidth / 4) * tx, (moveEvent.clientY - window.innerHeight / 4) * ty);
        };
        this.addEventListener("mousemove", mouseMoveHandler);
      } else {
        // Remove event listeners and close fullscreen view
        if (mouseMoveHandler) {
          this.removeEventListener("mousemove", mouseMoveHandler);
        }
        this.removeEventListener("click", clickHandler);
        this.remove();
        document.body.style.overflow = "visible";
        el.style.display = "none";
      }
    };
    
    // Add click handler for zoom toggle
    this.addEventListener("click", clickHandler);
    
    // Add the image to DOM and show fullscreen container
    el.innerHTML = ''; // Clear previous content
    el.appendChild(this);
    el.style.display = "block";
    document.body.style.overflow = "hidden";
  };
  
  img.onerror = function() {
    console.error(`Failed to load image: ${name}`);
  };
  
  // Start loading the image
  img.src = name;
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

/**
 * Set up listener for device pixel ratio changes
 */
function setupPixelRatioChangeListener() {
  if (window.removePiselRatio != null) {
    window.removePiselRatio();
  }
  
  const mqString = `(resolution: ${window.devicePixelRatio}dppx)`;
  const media = matchMedia(mqString);
  media.addEventListener("change", updatePixelRatio);
  
  window.removePiselRatio = () => {
    media.removeEventListener("change", updatePixelRatio);
  };
}

/**
 * Updates pixel ratio-dependent SVG filters
 * This function adjusts kernel matrices for feConvolveMatrix filters
 * based on the current device pixel ratio
 */
export function updatePixelRatio() {
  // Update kernel matrices based on pixel ratio
  Array.from(document.getElementsByTagName("feConvolveMatrix")).forEach(element => {
    if (typeof element.getAttribute('id') === 'string') {
      const elt = element.getAttribute('id').split('_|_');
      if (elt.length === 2) {
        const s = elt[0] * window.devicePixelRatio;
        const maxCount = Math.ceil(s * 2);
        let kernelMatix = '';
        let divisor = 0;
        
        for (let i = -maxCount; i <= maxCount; i++) {
          const y = Math.floor(1000 * Math.exp((-i * i) / (s * s)));
          divisor += y;
          kernelMatix += ` ${y}`;
        }
        
        element.setAttribute('kernelMatrix', kernelMatix);
        element.setAttribute('divisor', divisor);
        
        if (elt[1] === 'y') {
          element.setAttribute('order', `1,${2 * maxCount + 1}`);
        } else {
          element.setAttribute('order', `${2 * maxCount + 1},1`);
        }
      }
    }
  });
}

/**
 * Generate a random sticker color
 * Returns a color from predefined palette or based on custom attributes
 * @param {number} index - Sticker index for variety
 * @param {HTMLElement} element - Sticker element that might have custom color settings
 * @returns {Object} - Object with hue, saturation, and lightness values
 */
function generateStickerColor(index, element) {
  // Seed for deterministic randomness
  const seed = index * 137 + 29; // Prime numbers for better distribution
  
  // Predefined sticker color palettes (paler than original)
  const stickerPalettes = [
    // Yellow/Gold palette
    { hue: 57, saturation: 85, lightness: 55 },
    // Blue palette
    { hue: 210, saturation: 60, lightness: 75 },
    // Pink/Rose palette
    { hue: 340, saturation: 50, lightness: 80 },
    // Green palette
    { hue: 120, saturation: 40, lightness: 78 },
    // Orange/Peach palette
    { hue: 32, saturation: 70, lightness: 75 },
    // Purple palette
    { hue: 280, saturation: 40, lightness: 80 },
    // Teal palette
    { hue: 170, saturation: 50, lightness: 70 }
  ];

  // Check for custom color settings
  if (element && element.dataset) {
    // Check for explicit hue value
    const customHue = parseInt(element.dataset.hue);
    if (!isNaN(customHue)) {
      return {
        hue: customHue,
        saturation: 60,
        lightness: 70
      };
    }
    
    // Check for hue range
    const minHue = parseInt(element.dataset.minHue);
    const maxHue = parseInt(element.dataset.maxHue);
    if (!isNaN(minHue) && !isNaN(maxHue)) {
      const range = maxHue - minHue;
      // Use seed for deterministic random hue within range
      const hue = minHue + (seed % range);
      return {
        hue,
        saturation: 60,
        lightness: 70
      };
    }
  }
  
  // Default behavior: first sticker always gets yellow, others get random palette
  if (index === 0) {
    return stickerPalettes[0];
  }
  
  // For other stickers, select palette based on index or seed
  const paletteIndex = 1 + (seed % (stickerPalettes.length - 1));
  return stickerPalettes[paletteIndex];
}

/**
 * Applies random effects to stickers and buttons
 * Uses realistic colors and ensures good contrast
 */
export function applyRandomEffects() {
  console.log("Applying random effects to stickers, pins, and buttons");
  
  // Get all stickers
  const stickers = document.querySelectorAll('.sticker');
  console.log(`Found ${stickers.length} stickers to colorize`);
  
  // Store sticker colors to use for buttons later
  const stickerColors = [];
  
  // Process each sticker
  stickers.forEach((sticker, index) => {
    // Random rotation between -4 and 4 degrees for natural look
    const randomRotation = (Math.random() * 8) - 4;
    sticker.style.transform = `rotate(${randomRotation}deg)`;
    
    // Get color based on index or custom attributes
    const color = generateStickerColor(index, sticker);
    stickerColors.push(color);
    
    // Store color in the element's dataset for reference
    sticker.dataset.appliedHue = color.hue;
    sticker.dataset.appliedSaturation = color.saturation;
    sticker.dataset.appliedLightness = color.lightness;
    
    // Apply sticker background with gradient
    sticker.style.background = `linear-gradient(
      hsla(${color.hue}, ${color.saturation}%, 38%, 0.25) 70px, 
      hsla(${color.hue}, ${color.saturation}%, 28%, 0.25), 
      hsla(${color.hue}, ${color.saturation}%, 99%, 0) 75px),
      hsla(${color.hue}, ${color.saturation}%, ${color.lightness}%, 0.55)`;
    
    console.log(`Sticker ${index}: Applied hue=${color.hue}, sat=${color.saturation}, light=${color.lightness}`);

    // Find and colorize pins within the sticker
    colorizeStrickerPins(sticker, color);
  });
  
  // Apply colors to buttons based on sticker colors
  colorizeButtons(stickerColors);
}

/**
 * Colorize all pins within a sticker - direct approach working with SVG definition
 * @param {HTMLElement} sticker - The sticker element
 * @param {Object} stickerColor - The color of the sticker
 */
function colorizeStrickerPins(sticker, stickerColor) {
  // Generate random pin hue that differs from sticker hue by at least 20 units
  let pinHue;
  let hueDiff;
  
  do {
    pinHue = Math.floor(Math.random() * 360);
    hueDiff = Math.min(
      Math.abs(pinHue - stickerColor.hue),
      360 - Math.abs(pinHue - stickerColor.hue)
    );
  } while (hueDiff < 20);
  
  console.log(`Sticker hue: ${stickerColor.hue}, Pin hue: ${pinHue} (diff: ${hueDiff})`);
  
  // Find pins in this sticker by use reference
  const pinUses = sticker.querySelectorAll('use[href="#pin"]');
  
  // Direct approach: find the pin definition once
  const pinDef = document.getElementById('pin');
  if (pinDef) {
    // Find all elements with pinColor class in the definition
    const pinColorElements = pinDef.querySelectorAll('.pinColor');
    
    // Store original colors to restore later
    if (!pinDef._originalColors) {
      pinDef._originalColors = [];
      pinColorElements.forEach(el => {
        // Store original attributes
        if (el.hasAttribute('fill')) {
          pinDef._originalColors.push({
            element: el,
            attribute: 'fill',
            value: el.getAttribute('fill')
          });
        }
        if (el.hasAttribute('stop-color')) {
          pinDef._originalColors.push({
            element: el,
            attribute: 'stop-color',
            value: el.getAttribute('stop-color')
          });
        }
      });
    }
    
    // Temporarily modify the original definition
    pinColorElements.forEach(el => {
      if (el.hasAttribute('fill') && !el.getAttribute('fill').startsWith('url(')) {
        el.setAttribute('fill', `hsl(${pinHue}, 90%, 45%)`);
      }
      if (el.hasAttribute('stop-color')) {
        el.setAttribute('stop-color', `hsl(${pinHue}, 90%, 45%)`);
      }
    });
    
    // Force a repaint of the pins
    pinUses.forEach(use => {
      use.style.display = 'none';
      setTimeout(() => { use.style.display = ''; }, 0);
    });
    
    // Reset original colors with a small delay
    setTimeout(() => {
      if (pinDef._originalColors) {
        pinDef._originalColors.forEach(item => {
          item.element.setAttribute(item.attribute, item.value);
        });
      }
    }, 100);
  }
}

/**
 * Changes only the hue of a color while preserving saturation and lightness
 * Works with various color formats (hex, rgb, hsl)
 * @param {string} originalColor - Original color in any valid CSS format
 * @param {number} newHue - New hue value (0-360)
 * @param {number} lightnessAdjust - Optional adjustment to lightness (-100 to 100)
 * @returns {string} - New color in appropriate format
 */
function changeHuePreserveOthers(originalColor, newHue, lightnessAdjust = 0) {
  // Handle empty or invalid colors
  if (!originalColor || originalColor === 'none' || originalColor === 'transparent') {
    return originalColor;
  }
  
  // Parse color to RGB
  let r, g, b;
  
  // Handle different color formats
  if (originalColor.startsWith('#')) {
    // Handle hex format
    const hex = originalColor.substring(1);
    const bigint = parseInt(hex, 16);
    r = (bigint >> 16) & 255;
    g = (bigint >> 8) & 255;
    b = bigint & 255;
  } else if (originalColor.startsWith('rgb')) {
    // Handle rgb/rgba format
    const matches = originalColor.match(/rgba?\((\d+),\s*(\d+),\s*(\d+)(?:,\s*[\d.]+)?\)/);
    if (matches) {
      r = parseInt(matches[1]);
      g = parseInt(matches[2]);
      b = parseInt(matches[3]);
    } else {
      // Can't parse, return original
      return originalColor;
    }
  } else if (originalColor.startsWith('hsl')) {
    // Handle hsl/hsla format - replace the hue value and adjust lightness
    const matches = originalColor.match(/hsla?\((\d+),\s*([\d.]+)%,\s*([\d.]+)%(?:,\s*([\d.]+))?\)/);
    if (matches) {
      // Extract values
      const oldHue = parseInt(matches[1]);
      const sat = parseFloat(matches[2]);
      let light = parseFloat(matches[3]);
      const alpha = matches[4] ? parseFloat(matches[4]) : 1;
      
      // Adjust lightness, ensuring it stays between 0-100
      light = Math.max(0, Math.min(100, light + lightnessAdjust));
      
      // Return new HSL/HSLA with updated hue and lightness
      if (matches[4]) {
        return `hsla(${newHue}, ${sat}%, ${light}%, ${alpha})`;
      } else {
        return `hsl(${newHue}, ${sat}%, ${light}%)`;
      }
    } else {
      // Can't parse, return original
      return originalColor;
    }
  } else {
    // Unknown format, return original
    return originalColor;
  }
  
  // Convert RGB to HSL
  r /= 255;
  g /= 255;
  b /= 255;
  
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  let h, s, l = (max + min) / 2;
  
  if (max === min) {
    // Achromatic (gray)
    h = 0;
    s = 0;
  } else {
    const d = max - min;
    s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
    
    switch (max) {
      case r: h = (g - b) / d + (g < b ? 6 : 0); break;
      case g: h = (b - r) / d + 2; break;
      case b: h = (r - g) / d + 4; break;
    }
    
    h /= 6;
  }
  
  // Convert to degrees, use the new hue, keep original saturation
  h = newHue;
  s = Math.round(s * 100);
  
  // Apply lightness adjustment, ensuring it stays between 0-100
  l = Math.round(l * 100) + lightnessAdjust;
  l = Math.max(0, Math.min(100, l));
  
  // Return new HSL color
  return `hsl(${h}, ${s}%, ${l}%)`;
}

/**
 * Apply colors to all buttons based on sticker colors
 * @param {Array} stickerColors - Array of sticker colors
 */
function colorizeButtons(stickerColors) {
  // Get all buttons
  const buttons = document.querySelectorAll('.button');
  console.log(`Found ${buttons.length} buttons to colorize`);
  
  if (buttons.length === 0 || stickerColors.length === 0) {
    return;
  }
  
  // Apply colors to each button
  buttons.forEach((button, index) => {
    // Get a base color from the sticker colors (cycle through them)
    const baseColor = stickerColors[index % stickerColors.length];
    
    // Create a color 20 units away from the base color
    const buttonHue = (baseColor.hue + 20) % 360;
    
    // Create button color CSS
    const buttonColorStyle = `hsla(${buttonHue}, ${baseColor.saturation}%, ${baseColor.lightness}%, 0.5)`;
    
    // Apply the color
    button.style.backgroundColor = buttonColorStyle;
    
    console.log(`Button ${index}: Applied hue=${buttonHue} (based on sticker hue ${baseColor.hue})`);
  });
}

/**
 * Force recoloring of all pins in the document
 * Each pin gets a random hue based on its sticker, preserving gradients and shadows
 */
function forceRecolorAllPins() {
  // Get all stickers first
  const stickers = document.querySelectorAll('.sticker');
  console.log(`Found ${stickers.length} stickers to process their pins`);
  
  // Process pins in each sticker
  stickers.forEach((sticker, stickerIndex) => {
    // Get sticker's hue from dataset if available
    const stickerHue = parseInt(sticker.dataset.appliedHue || sticker.dataset.hue || 0);
    
    // Find all pins within this sticker
    const pins = sticker.querySelectorAll('use[href="#pin"]');
    console.log(`Sticker ${stickerIndex} (hue ${stickerHue}) has ${pins.length} pins`);
    
    // Process each pin in this sticker
    pins.forEach((pin, pinIndex) => {
      // Generate random hue that differs from sticker hue by at least 20 units
      let pinHue;
      let hueDiff;
      
      do {
        pinHue = Math.floor(Math.random() * 360);
        // Calculate minimum distance on the color wheel (considering it's circular)
        hueDiff = Math.min(
          Math.abs(pinHue - stickerHue),
          360 - Math.abs(pinHue - stickerHue)
        );
      } while (hueDiff < 20);
      
      console.log(`Pin ${pinIndex} in sticker ${stickerIndex}: Applying hue ${pinHue} (differs from sticker by ${hueDiff})`);
      
      // Set data attribute for debugging
      pin.dataset.appliedHue = pinHue;
      
      // Since pins are defined using <use> elements that reference a shared <defs> element,
      // we need to create custom styles to override the colors for each specific pin
      
      // Create a unique class name for this pin
      const uniqueClassName = `pin-${stickerIndex}-${pinIndex}`;
      pin.classList.add(uniqueClassName);
      
      // Create or update a style element for this pin
      let styleEl = document.getElementById(`style-${uniqueClassName}`);
      if (!styleEl) {
        styleEl = document.createElement('style');
        styleEl.id = `style-${uniqueClassName}`;
        document.head.appendChild(styleEl);
      }
      
      // Create CSS that targets all .pinColor elements within this specific pin
      // This allows us to selectively override colors just for this pin
      styleEl.textContent = `
        .${uniqueClassName} .pinColor[fill]:not([fill^="url"]) {
          fill: hsl(${pinHue}, var(--pin-saturation, 90%), var(--pin-lightness, 40%)) !important;
        }
        .${uniqueClassName} .pinColor[stop-color] {
          stop-color: hsl(${pinHue}, var(--pin-saturation, 90%), var(--pin-lightness, 40%)) !important;
        }
      `;
    });
  });
  
  // Look for pins outside stickers as fallback
  const standalonesPins = Array.from(document.querySelectorAll('use[href="#pin"]')).filter(
    pin => !pin.closest('.sticker')
  );
  
  console.log(`Found ${standalonesPins.length} standalone pins outside of stickers`);
  
  standalonesPins.forEach((pin, index) => {
    // Generate a completely random hue for standalone pins
    const pinHue = Math.floor(Math.random() * 360);
    
    console.log(`Standalone pin ${index}: Applying hue ${pinHue}`);
    
    // Create a unique class for this pin
    const uniqueClassName = `standalone-pin-${index}`;
    pin.classList.add(uniqueClassName);
    
    // Create or update style element
    let styleEl = document.getElementById(`style-${uniqueClassName}`);
    if (!styleEl) {
      styleEl = document.createElement('style');
      styleEl.id = `style-${uniqueClassName}`;
      document.head.appendChild(styleEl);
    }
    
    // Create CSS for this pin
    styleEl.textContent = `
      .${uniqueClassName} .pinColor[fill]:not([fill^="url"]) {
        fill: hsl(${pinHue}, var(--pin-saturation, 90%), var(--pin-lightness, 40%)) !important;
      }
      .${uniqueClassName} .pinColor[stop-color] {
        stop-color: hsl(${pinHue}, var(--pin-saturation, 90%), var(--pin-lightness, 40%)) !important;
      }
    `;
  });
  
  // Define CSS variables for pin colors (can be customized in future)
  const rootStyle = document.getElementById('pin-root-style') || document.createElement('style');
  rootStyle.id = 'pin-root-style';
  rootStyle.textContent = `
    :root {
      --pin-saturation: 90%;
      --pin-lightness: 40%;
    }
  `;
  document.head.appendChild(rootStyle);
  
  // Force a redraw to ensure colors update
  const allSvgElements = document.querySelectorAll('svg');
  allSvgElements.forEach(svg => {
    // Trigger a reflow
    const display = svg.style.display;
    svg.style.display = 'none';
    setTimeout(() => {
      svg.style.display = display;
    }, 0);
  });
}

/**
 * Creates a mutation observer to apply effects to dynamically added content
 */
function initDynamicContentObserver() {
  // Create a mutation observer to watch for added stickers and pins
  const observer = new MutationObserver(mutations => {
    let needsUpdate = false;
    
    mutations.forEach(mutation => {
      if (mutation.type === 'childList' && mutation.addedNodes.length) {
        // Check if any of the added nodes are stickers/pins or contain stickers/pins
        mutation.addedNodes.forEach(node => {
          if (node.nodeType === Node.ELEMENT_NODE) {
            if (node.classList && node.classList.contains('sticker')) {
              needsUpdate = true;
            } else if (node.querySelector && node.querySelector('.sticker, .pinColor')) {
              needsUpdate = true;
            }
          }
        });
      }
    });
    
    if (needsUpdate) {
      applyRandomEffects();
    }
  });
  
  // Start observing the document body for changes
  observer.observe(document.body, {
    childList: true,
    subtree: true
  });
  
  // Force recolor of all pins after a short delay
  setTimeout(() => {
    forceRecolorAllPins();
  }, 500);
}

// Add to window object for direct access
window.forceApplyRandomEffects = applyRandomEffects;
window.forceRecolorAllPins = forceRecolorAllPins;