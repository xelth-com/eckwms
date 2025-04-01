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
  if (picsArray.length && myEl && myEl.getAttribute('src') === '') {
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
 * Applies random effects to stickers and buttons using simple hue-rotate approach
 * This matches the original implementation with adjustments for colors
 */
export function applyRandomEffects() {
  console.log("Applying random effects to stickers and pins");
  
  // Get all stickers for reference
  const stickers = Array.from(document.getElementsByClassName("sticker"));
  
  // Process each sticker
  stickers.forEach((sticker, index) => {
    // Random rotation between -4.5 and 4.5 degrees
    const randomRotation = Math.random() * 9 - 4.5;
    sticker.style.transform = `rotate(${randomRotation}deg)`;
    
    // Check if color is specified in HTML
    let stickerHue = parseInt(sticker.dataset.hue);
    let stickerSaturation = parseInt(sticker.dataset.saturation);
    let stickerLightness = parseInt(sticker.dataset.lightness);
    
    // Default first sticker to yellow if not specified
    if (index === 0 && isNaN(stickerHue)) {
      stickerHue = 57; // Yellow
      stickerSaturation = 50;
      stickerLightness = 70;
    }
    
    // If hue is not specified in HTML, generate random hue
    if (isNaN(stickerHue)) {
      stickerHue = Math.floor(Math.random() * 360);
    }
    
    // Use provided values or defaults
    stickerSaturation = !isNaN(stickerSaturation) ? stickerSaturation : 40; // Less saturated
    stickerLightness = !isNaN(stickerLightness) ? stickerLightness : 80; // Much lighter
    
    // Apply sticker background color
    sticker.style.background = `linear-gradient(
      hsla(${stickerHue}, ${stickerSaturation}%, 38%, 0.25) 70px, 
      hsla(${stickerHue}, ${stickerSaturation}%, 28%, 0.25), 
      hsla(${stickerHue}, ${stickerSaturation}%, 99%, 0) 75px),
      hsla(${stickerHue}, ${stickerSaturation}%, ${stickerLightness}%, 0.55)`;
    
    // Store the applied hue in dataset
    sticker.dataset.appliedHue = stickerHue;
    sticker.dataset.appliedSaturation = stickerSaturation;
    sticker.dataset.appliedLightness = stickerLightness;
    
    console.log(`Sticker ${index}: Applied hue=${stickerHue}, saturation=${stickerSaturation}%, lightness=${stickerLightness}%`);
    
    // Apply hue rotation to all SVGs within the sticker (pins)
    Array.from(sticker.getElementsByTagName('svg')).forEach(svg => {
      // Check if pin color is specified in HTML
      const specifiedPinHue = parseInt(svg.dataset.hue);
      
      if (!isNaN(specifiedPinHue)) {
        // Use specified hue for the pin
        svg.style.filter = `hue-rotate(${specifiedPinHue}deg)`;
        console.log(`Pin in sticker ${index}: Using specified hue rotation=${specifiedPinHue}°`);
      } else {
        // Generate random hue angle
        let hueAngle = Math.random() * 360;
        
        // Ensure the pin color differs from sticker color by at least 30 degrees
        const hueDiff = Math.min(
          Math.abs(hueAngle - stickerHue),
          360 - Math.abs(hueAngle - stickerHue)
        );
        
        if (hueDiff < 30) {
          // Shift by 60 degrees if too close to sticker color
          hueAngle = (stickerHue + 60 + Math.random() * 30) % 360;
        }
        
        // Apply hue rotation filter 
        svg.style.filter = `hue-rotate(${hueAngle}deg)`;
        
        console.log(`Pin in sticker ${index}: Applied hue rotation=${hueAngle}°`);
      }
    });
  });
  
  // Colorize buttons to complement stickers
  colorizeButtons(stickers);
}

/**
 * Apply colors to all buttons
 * @param {Array} stickers - Array of sticker elements to reference their colors
 */
function colorizeButtons(stickers) {
  // Get all buttons
  const buttons = document.querySelectorAll('.button');
  console.log(`Found ${buttons.length} buttons to colorize`);
  
  if (buttons.length === 0) {
    return;
  }
  
  // Apply colors to each button
  buttons.forEach((button, index) => {
    // Check if button color is specified in HTML
    const specifiedHue = parseInt(button.dataset.hue);
    const specifiedSaturation = parseInt(button.dataset.saturation);
    const specifiedLightness = parseInt(button.dataset.lightness);
    
    if (!isNaN(specifiedHue)) {
      // Use specified color for the button
      const saturation = !isNaN(specifiedSaturation) ? specifiedSaturation : 70;
      const lightness = !isNaN(specifiedLightness) ? specifiedLightness : 40; // Darker
      
      button.style.backgroundColor = `hsla(${specifiedHue}, ${saturation}%, ${lightness}%, 0.5)`;
      console.log(`Button ${index}: Using specified hue=${specifiedHue}, saturation=${saturation}%, lightness=${lightness}%`);
    } else {
      // Reference nearest sticker color if available
      let referenceHue = 0;
      if (stickers && stickers.length > 0) {
        // Use color from sticker at same index or the last sticker
        const refIndex = Math.min(index, stickers.length - 1);
        const refSticker = stickers[refIndex];
        
        if (refSticker && refSticker.dataset.appliedHue) {
          referenceHue = parseInt(refSticker.dataset.appliedHue);
        }
      }
      
      // Create a color different from the reference
      const buttonHue = (referenceHue + 45 + Math.floor(Math.random() * 90)) % 360;
      const saturation = !isNaN(specifiedSaturation) ? specifiedSaturation : 70;
      const lightness = !isNaN(specifiedLightness) ? specifiedLightness : 40; // Darker buttons
      
      button.style.backgroundColor = `hsla(${buttonHue}, ${saturation}%, ${lightness}%, 0.5)`;
      console.log(`Button ${index}: Applied hue=${buttonHue}, saturation=${saturation}%, lightness=${lightness}%`);
    }
  });
}

/**
 * Creates a mutation observer to apply effects to dynamically added content
 */
function initDynamicContentObserver() {
  // Create a mutation observer to watch for added stickers
  const observer = new MutationObserver(mutations => {
    let needsUpdate = false;
    
    mutations.forEach(mutation => {
      if (mutation.type === 'childList' && mutation.addedNodes.length) {
        // Check if any of the added nodes are stickers or contain stickers
        mutation.addedNodes.forEach(node => {
          if (node.nodeType === Node.ELEMENT_NODE) {
            if (node.classList && node.classList.contains('sticker')) {
              needsUpdate = true;
            } else if (node.querySelector && node.querySelector('.sticker')) {
              needsUpdate = true;
            }
          }
        });
      }
    });
    
    if (needsUpdate) {
      // Only apply effects to new stickers
      const unprocessedStickers = Array.from(document.getElementsByClassName("sticker"))
        .filter(sticker => !sticker.dataset.appliedHue);
      
      if (unprocessedStickers.length > 0) {
        // Get all stickers for reference (including processed ones)
        const allStickers = Array.from(document.getElementsByClassName("sticker"));
        
        unprocessedStickers.forEach((sticker, localIndex) => {
          // Get global index for consistent coloring
          const globalIndex = allStickers.indexOf(sticker);
          
          // Random rotation
          const randomRotation = Math.random() * 9 - 4.5;
          sticker.style.transform = `rotate(${randomRotation}deg)`;
          
          // Check if color is specified in HTML
          let stickerHue = parseInt(sticker.dataset.hue);
          let stickerSaturation = parseInt(sticker.dataset.saturation);
          let stickerLightness = parseInt(sticker.dataset.lightness);
          
          // Default first sticker to yellow if not specified and it's the first sticker
          if (globalIndex === 0 && isNaN(stickerHue)) {
            stickerHue = 57; // Yellow
            stickerSaturation = 50;
            stickerLightness = 70;
          }
          
          // If hue is not specified in HTML, generate random hue
          if (isNaN(stickerHue)) {
            stickerHue = Math.floor(Math.random() * 360);
          }
          
          // Use provided values or defaults
          stickerSaturation = !isNaN(stickerSaturation) ? stickerSaturation : 40; // Less saturated
          stickerLightness = !isNaN(stickerLightness) ? stickerLightness : 80; // Much lighter
          
          // Apply sticker background color
          sticker.style.background = `linear-gradient(
            hsla(${stickerHue}, ${stickerSaturation}%, 38%, 0.25) 70px, 
            hsla(${stickerHue}, ${stickerSaturation}%, 28%, 0.25), 
            hsla(${stickerHue}, ${stickerSaturation}%, 99%, 0) 75px),
            hsla(${stickerHue}, ${stickerSaturation}%, ${stickerLightness}%, 0.55)`;
          
          // Store the applied hue
          sticker.dataset.appliedHue = stickerHue;
          sticker.dataset.appliedSaturation = stickerSaturation;
          sticker.dataset.appliedLightness = stickerLightness;
          
          // Apply hue rotation to SVGs
          Array.from(sticker.getElementsByTagName('svg')).forEach(svg => {
            // Check if pin color is specified
            const specifiedPinHue = parseInt(svg.dataset.hue);
            
            if (!isNaN(specifiedPinHue)) {
              // Use specified hue
              svg.style.filter = `hue-rotate(${specifiedPinHue}deg)`;
            } else {
              // Generate random hue angle
              let hueAngle = Math.random() * 360;
              
              // Ensure the pin color differs from sticker color by at least 30 degrees
              const hueDiff = Math.min(
                Math.abs(hueAngle - stickerHue),
                360 - Math.abs(hueAngle - stickerHue)
              );
              
              if (hueDiff < 30) {
                // Shift by 60 degrees if too close to sticker color
                hueAngle = (stickerHue + 60 + Math.random() * 30) % 360;
              }
              
              // Apply hue rotation filter
              svg.style.filter = `hue-rotate(${hueAngle}deg)`;
            }
          });
        });
        
        // Update buttons to match new colors
        colorizeButtons(allStickers);
      }
    }
  });
  
  // Start observing the document body for changes
  observer.observe(document.body, {
    childList: true,
    subtree: true
  });
}

// Add to window object for direct access
window.forceApplyRandomEffects = applyRandomEffects;