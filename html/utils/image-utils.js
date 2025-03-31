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
  applyRandomEffects();
  
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
    
    // Add click handler for zoom toggle
    this.addEventListener("click", tempFa = (event) => {
      if (this.style.cursor == 'zoom-in') {
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
        this.addEventListener("mousemove", tempFb = (event) => {
          el.scroll((event.clientX - window.innerWidth / 4) * tx, (event.clientY - window.innerHeight / 4) * ty);
        });
      } else {
        // Remove event listeners and close fullscreen view
        this.removeEventListener("mousemove", tempFb);
        this.removeEventListener("click", tempFa);
        this.remove();
        document.body.style.overflow = "visible";
        el.style.display = "none";
      }
    });
    
    // Add the image to DOM and show fullscreen container
    el.appendChild(this);
    el.style.display = "block";
    document.body.style.overflow = "hidden";
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
          const y = ~~(1000 * Math.exp((-i * i) / (s * s)));
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
 * Applies random colors to pins and random rotations to stickers
 * This creates a "hand-placed" appearance with varied angles and colors
 */
export function applyRandomEffects() {
  console.log("Applying random effects to stickers and pins");
  
  // Apply random rotation to stickers
  document.querySelectorAll('.sticker').forEach(sticker => {
    // Random rotation between -4.5 and 4.5 degrees
    const randomRotation = Math.random() * 9 - 4.5;
    sticker.style.transform = `rotate(${randomRotation}deg)`;
    
    // Apply random colors to the SVG elements inside stickers
    sticker.querySelectorAll('svg').forEach(pinSvg => {
      // Random hue rotation between 0 and 360 degrees
      let hueAngle = Math.random() * 360;
      
      // Avoid certain hue ranges that might not look good
      if (hueAngle < (57 + 25) && hueAngle > (57 - 15)) {
        hueAngle -= 40;
      }
      
      pinSvg.style.filter = `hue-rotate(${hueAngle}deg)`;
    });
  });
  
  // Apply random colors to pinColor class elements
  document.querySelectorAll('.pinColor').forEach(pinElement => {
    // Generate a random hue (keeping saturation and lightness consistent)
    const hue = Math.floor(Math.random() * 360);
    
    // Set the fill or stop-color attribute based on element type
    if (pinElement.tagName.toLowerCase() === 'stop') {
      // For gradient stops, maintain their lightness differences
      const originalColor = pinElement.getAttribute('stop-color');
      if (originalColor && originalColor.includes('hsl')) {
        // Extract the lightness value from the original HSL
        const lightnessMatch = originalColor.match(/hsl\(\s*\d+\s*,\s*\d+%\s*,\s*(\d+)%\s*\)/);
        if (lightnessMatch && lightnessMatch[1]) {
          const lightness = lightnessMatch[1];
          // Set new color with same lightness but random hue
          pinElement.setAttribute('stop-color', `hsl(${hue}, 100%, ${lightness}%)`);
        }
      }
    } else if (pinElement.hasAttribute('fill')) {
      // For elements with fill attribute
      if (pinElement.getAttribute('fill').startsWith('url(#')) {
        // Skip elements with gradient fills
        return;
      }
      pinElement.setAttribute('fill', `hsl(${hue}, 100%, 50%)`);
    } else {
      // For elements with style.fill
      pinElement.style.fill = `hsl(${hue}, 100%, 50%)`;
    }
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
}