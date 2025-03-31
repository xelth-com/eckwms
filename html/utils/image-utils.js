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
 * Get a realistic sticker color with proper hue, saturation, and lightness
 * @param {number} index - Sticker index for variety
 * @returns {Object} - Object with hue, saturation, and lightness values
 */
function getStickerColor(index) {
  // Predefined realistic sticker colors - pastels for all except first one
  const stickerColors = [
    { hue: 57, saturation: 96, lightness: 48 },   // Default yellow (first sticker)
    { hue: 190, saturation: 70, lightness: 65 },  // Light blue
    { hue: 340, saturation: 60, lightness: 75 },  // Pink
    { hue: 120, saturation: 50, lightness: 70 },  // Light green
    { hue: 40, saturation: 80, lightness: 65 },   // Orange/peach
    { hue: 280, saturation: 50, lightness: 75 },  // Light purple
    { hue: 15, saturation: 70, lightness: 65 }    // Salmon
  ];

  // For first sticker, always use the default
  if (index === 0) {
    return stickerColors[0];
  }
  
  // For other stickers, choose a color avoiding the first (default)
  // Pick a color based on index or random if index is too large
  const colorIndex = index < stickerColors.length ? index : 
                   1 + Math.floor(Math.random() * (stickerColors.length - 1));
  
  return stickerColors[colorIndex];
}

/**
 * Get a pin color that contrasts well with the sticker
 * @param {Object} stickerColor - The sticker color properties
 * @returns {Object} - Object with hue, saturation, and lightness values
 */
function getPinColor(stickerColor) {
  // For pins, use a complementary color that's darker and more saturated
  const hue = (stickerColor.hue + 180) % 360;
  return {
    hue: hue,
    saturation: 90,  // More saturated
    lightness: 30    // Darker
  };
}

/**
 * Applies random rotations to stickers and appropriate colors to pins
 * Uses realistic colors for stickers and ensures good contrast with pins
 */
export function applyRandomEffects() {
  console.log("Applying random effects to stickers and pins");
  
  // First, get all stickers
  const stickers = document.querySelectorAll('.sticker');
  
  // Apply random rotation and color to each sticker
  stickers.forEach((sticker, index) => {
    // Random rotation between -4.5 and 4.5 degrees for all stickers
    const randomRotation = Math.random() * 9 - 4.5;
    sticker.style.transform = `rotate(${randomRotation}deg)`;
    
    // Get sticker color based on index
    const stickerColor = getStickerColor(index);
    
    // Store color info in dataset for debugging and future reference
    sticker.dataset.hue = stickerColor.hue;
    sticker.dataset.saturation = stickerColor.saturation;
    sticker.dataset.lightness = stickerColor.lightness;
    
    // Apply base sticker color through CSS
    sticker.style.background = `linear-gradient(
      hsla(${stickerColor.hue}, ${stickerColor.saturation}%, 38%, 0.3) 70px, 
      hsla(${stickerColor.hue}, ${stickerColor.saturation}%, 28%, 0.3), 
      hsla(${stickerColor.hue}, ${stickerColor.saturation}%, 99%, 0) 75px),
      hsla(${stickerColor.hue}, ${stickerColor.saturation}%, ${stickerColor.lightness}%, 0.65)`;
    
    // Get corresponding pin color
    const pinColor = getPinColor(stickerColor);
    
    // Apply colors to all SVG elements in the sticker
    const svgs = sticker.querySelectorAll('svg');
    svgs.forEach(svg => {
      // Apply color to pins
      const pinElements = svg.querySelectorAll('.pinColor');
      pinElements.forEach(pinElement => {
        // Apply pin color based on element type
        if (pinElement.tagName.toLowerCase() === 'stop') {
          // For gradient stops
          pinElement.setAttribute('stop-color', `hsl(${pinColor.hue}, ${pinColor.saturation}%, ${pinColor.lightness}%)`);
        } else if (pinElement.hasAttribute('fill') && !pinElement.getAttribute('fill').startsWith('url(#')) {
          // For elements with direct fill attribute (not gradients)
          pinElement.setAttribute('fill', `hsl(${pinColor.hue}, ${pinColor.saturation}%, ${pinColor.lightness}%)`);
        }
      });
    });
  });
  
  // Log debug info
  console.log(`Applied colors to ${stickers.length} stickers`);
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