/**
 * Home Content Module
 * Displays the main content of the homepage
 */

import { loadCSS, loadTemplate } from '/core/module-loader.js';
import { myFetch, picFromSet } from '/utils/fetch-utils.js';
import { fsPic } from '/utils/image-utils.js';

/**
 * Initialize home content
 * @param {HTMLElement} container - Container to render content into
 */
export async function init(container) {
  // Load required CSS
  await loadCSS('/content/home-content.css');
  
  // Load HTML template
  const html = await loadTemplate('/content/home-content.template.html');
  container.innerHTML = html;
  
  // Initialize the RMA form functionality
  initRmaForm();
  
  // Initialize event listeners
  initEventListeners();
  
  // Load images with fallbacks
  initImages();
}

/**
 * Initialize RMA form functionality
 */
function initRmaForm() {
  const snInput = document.getElementById("snInput");
  if (snInput) {
    snInput.style.background = `center / 100% 100% no-repeat url(data:image/svg+xml;charset=utf-8;base64,${btoa(window.backSvg3)})`;
    
    // Initialize input field
    snInput.addEventListener("keydown", (event) => {
      if (event.key === "Enter") {
        event.preventDefault();
        if (snInput.value === "0000000") {
          document.getElementById("snInputText").innerText = navigator.userAgent;
        }

        myFetch(snInput.value, "snInput", 'output2');
        snInput.value = '';
      }
    });

    snInput.addEventListener("keyup", (event) => {
      if (/^\d{7}$/.test(snInput.value)) {
        snInput.style.color = "rgb(0, 100, 50)";
        document.getElementById("snInputTextUp").innerHTML = "<br><br>can be the M3 device serial number<br><br>";
      } else if (/^RMA\d{10}[A-Za-z0-9]{2}$/.test(snInput.value)) {
        snInput.style.color = "rgb(0, 100, 50)";
        document.getElementById("snInputTextUp").innerHTML = "<br><br>can be the M3 RMA serial number<br><br>";
      } else if (/^\d{4}[A-Za-z0-9]\d{7}$/.test(snInput.value)) {
        snInput.style.color = "rgb(0, 100, 50)";
        document.getElementById("snInputTextUp").innerHTML = "<br><br>can be the M3 replacement part number<br><br>";
      } else {
        snInput.style.color = "rgba(26, 26, 147, 0.7)";
      }
    });
    
    // Focus on input field after load
    setTimeout(() => { 
      snInput.focus({ preventScroll: true }); 
    }, 1000);
  }
  
  // Check for existing JWT token and initialize
  if (localStorage.getItem("jwt")) {
    myFetch('', 'startInput');
  }
}

/**
 * Initialize event listeners for interactive elements
 */
function initEventListeners() {
  // RMA form button event
  const rmaButton = document.getElementById('rmaButton');
  if (rmaButton) {
    rmaButton.addEventListener('click', () => {
      myFetch('rmaGenerate', 'rmaButton', 'output2', '', '/rma/generate');
    });
  }
}

/**
 * Initialize images with fallbacks
 */
function initImages() {
  const mainPic = document.getElementById('mainPic');
  if (mainPic && mainPic.getAttribute('src') === '') {
    picFromSet('mainPic', ['ul20', 'sm15']);
  }
}

/**
 * Post-initialization tasks, run after the DOM is updated
 */
export function postInit() {
  // Any additional initialization after DOM update
}

// Add these to the window object to allow inline handlers to work
window.myFetch = myFetch;
window.picFromSet = picFromSet;
window.fsPic = fsPic;
