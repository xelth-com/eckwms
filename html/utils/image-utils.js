/**
 * Image Utilities
 * Handles image-related operations
 */

/**
 * Shows an image in fullscreen mode
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
 * Updates pixel ratio-dependent effects
 */
export function updatePixelRatio() {
  if (window.removePiselRatio != null) {
    window.removePiselRatio();
  }
  
  const mqString = `(resolution: ${window.devicePixelRatio}dppx)`;
  const media = matchMedia(mqString);
  media.addEventListener("change", updatePixelRatio);
  
  window.removePiselRatio = () => {
    media.removeEventListener("change", updatePixelRatio);
  };

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
 * Initializes image effects
 */
export function initImageEffects() {
  // Reference to device pixel ratio
  window.devicePixelRatioWas = window.devicePixelRatio;
  
  // Update SVG filters based on pixel ratio
  updatePixelRatio();
  
  // Add random rotation to stickers
  Array.from(document.getElementsByClassName("sticker")).forEach(element => {
    element.style.transform = `rotate(${Math.random() * 9 - 4.5}deg)`;
    Array.from(element.getElementsByTagName('svg')).forEach(innerElement => {
      let hueAngle = Math.random() * 360;
      if (hueAngle < (57 + 25) && hueAngle > (57 - 15)) {
        hueAngle -= 40;
      }
      innerElement.style.filter = `hue-rotate(${hueAngle}deg)`;
    });
  });
}

// Initialize image effects when the module loads
document.addEventListener('DOMContentLoaded', initImageEffects);

// Export for global access
window.fsPic = fsPic;
window.updatePixelRatio = updatePixelRatio;
