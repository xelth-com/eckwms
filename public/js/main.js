/**
 * Main application JavaScript
 * This file coordinates all frontend functionality
 */
import apiService from './services/api.js';
import authService from './services/auth.js';
import { setupMenus } from './components/menu.js';
import { setupForms } from './components/forms.js';
import { setupModals } from './components/modal.js';

// Global application state
const appState = {
  currentLanguage: 'en',
  isLoggedIn: false,
  userRole: null,
  activeMenus: [],
  searchMode: 'serial', // 'serial' or 'rma'
};

/**
 * Initialize the application
 */
function initApp() {
  // Check for authentication
  const token = localStorage.getItem('jwt');
  if (token) {
    try {
      const payload = authService.verifyJWT(token);
      appState.isLoggedIn = true;
      
      if (payload.u) {
        appState.userRole = payload.a || 'user';
        document.getElementById('mainMenu1').innerHTML = `
          ${payload.u}
          <div hidden>
            <form onsubmit="event.preventDefault();searchItem(this.elements[0].value);">
              <input id="logInput" class="textM3" placeholder="SN, RMA, box, place..." autocomplete="off" type="text" inputmode="text" />
            </form>
            <form onsubmit="event.preventDefault();exportCsv();">
              <input id="logExp" class="button" value="export CSV" autocomplete="off" type="submit" />
            </form>
            <form onsubmit="event.preventDefault();logout();">
              <input id="logOut" class="button" value="logout" autocomplete="off" type="submit" />
            </form>
          </div>`;
      }
      
      // Initialize home screen with authenticated content
      apiService.makeRequest('', 'startInput', 'output1')
        .then(html => {
          document.getElementById('output1').innerHTML = html;
          setupEventListeners();
        })
        .catch(error => {
          console.error('Failed to load initial content:', error);
        });
    } catch (error) {
      console.error('Invalid token:', error);
      localStorage.removeItem('jwt');
      appState.isLoggedIn = false;
    }
  } else {
    setupEventListeners();
  }
  
  // Setup UI components
  setupMenus();
  setupForms();
  setupModals();
  setupLanguageSelector();
}

/**
 * Set up event listeners
 */
function setupEventListeners() {
  // Search input
  const searchInput = document.getElementById('snInput');
  if (searchInput) {
    searchInput.addEventListener('keydown', handleSearchKeydown);
    searchInput.addEventListener('keyup', handleSearchKeyup);
    
    // Set initial focus
    setTimeout(() => searchInput.focus(), 500);
  }
  
  // RMA button
  const rmaButton = document.getElementById('rmaButton');
  if (rmaButton) {
    rmaButton.addEventListener('click', handleRmaButtonClick);
  }
  
  // Add responsive behavior
  window.addEventListener('resize', handleWindowResize);
  
  // Setup SVG filters for non-Chrome browsers
  setupSvgFilters();
}

/**
 * Handle search input keydown event
 * @param {KeyboardEvent} event - Keydown event
 */
function handleSearchKeydown(event) {
  if (event.key === 'Enter') {
    event.preventDefault();
    const query = event.target.value.trim();
    
    if (query) {
      searchItem(query);
      event.target.value = '';
    }
  }
}

/**
 * Handle search input keyup event
 * @param {KeyboardEvent} event - Keyup event
 */
function handleSearchKeyup(event) {
  const value = event.target.value.trim();
  const snInputTextUp = document.getElementById('snInputTextUp');
  
  if (/^\d{7}$/.test(value)) {
    event.target.style.color = "rgb(0, 100, 50)";
    snInputTextUp.innerHTML = "<br><br>This appears to be an M3 device serial number<br><br>";
  } else if (/^RMA\d{10}[A-Za-z0-9]{2}$/.test(value)) {
    event.target.style.color = "rgb(0, 100, 50)";
    snInputTextUp.innerHTML = "<br><br>This appears to be an M3 RMA number<br><br>";
  } else if (/^\d{4}[A-Za-z0-9]\d{7}$/.test(value)) {
    event.target.style.color = "rgb(0, 100, 50)";
    snInputTextUp.innerHTML = "<br><br>This appears to be an M3 replacement part number<br><br>";
  } else {
    event.target.style.color = "rgba(26, 26, 147, 0.7)";
    snInputTextUp.innerHTML = "<br><br>To check the repair status, please enter our RMA number or the device serial number.<br><br>";
  }
}

/**
 * Search for an item
 * @param {string} query - Search query
 */
function searchItem(query) {
  apiService.search(query)
    .then(html => {
      const outputElement = document.getElementById('output2');
      if (outputElement) {
        outputElement.innerHTML = html;
      }
    })
    .catch(error => {
      console.error('Search failed:', error);
      alert('Search failed. Please try again.');
    });
}

/**
 * Handle RMA button click
 * @param {MouseEvent} event - Click event
 */
function handleRmaButtonClick(event) {
  apiService.generateRmaForm()
    .then(html => {
      const outputElement = document.getElementById('output2');
      if (outputElement) {
        outputElement.innerHTML = html;
        
        // Set up form submission
        const rmaForm = document.getElementById('rmaForm');
        if (rmaForm) {
          rmaForm.addEventListener('submit', handleRmaFormSubmit);
        }
      }
    })
    .catch(error => {
      console.error('Failed to generate RMA form:', error);
      alert('Failed to generate RMA form. Please try again.');
    });
}

/**
 * Handle RMA form submission
 * @param {Event} event - Form submission event
 */
function handleRmaFormSubmit(event) {
  event.preventDefault();
  
  const form = event.target;
  apiService.submitForm(form, 'rmaForm', 'pdfRma')
    .then(blob => {
      // Create download link
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = `${form.elements.rma.value}.pdf`;
      document.body.appendChild(a);
      a.click();
      a.remove();
      URL.revokeObjectURL(url);
      
      // Reset form or redirect
      window.location.reload();
    })
    .catch(error => {
      console.error('Form submission failed:', error);
      alert('Form submission failed. Please try again.');
    });
}

/**
 * Export data as CSV
 */
function exportCsv() {
  apiService.exportCsv()
    .then(csv => {
      // Create download link
      const blob = new Blob([csv], { type: 'text/csv' });
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = `${Math.floor(Date.now() / 1000)}.csv`;
      document.body.appendChild(a);
      a.click();
      a.remove();
      URL.revokeObjectURL(url);
    })
    .catch(error => {
      console.error('CSV export failed:', error);
      alert('CSV export failed. Please try again.');
    });
}

/**
 * Log out the user
 */
function logout() {
  localStorage.removeItem('jwt');
  apiService.clearAuthToken();
  window.location.reload();
}

/**
 * Set up language selector
 */
function setupLanguageSelector() {
  const languageButtons = document.querySelectorAll('[id^="lang"]');
  languageButtons.forEach(button => {
    button.addEventListener('click', () => {
      const lang = button.getAttribute('href').slice(1);
      setLanguage(lang);
    });
  });
  
  // Set default language
  setLanguage('en');
}

/**
 * Set the application language
 * @param {string} lang - Language code
 */
function setLanguage(lang) {
  const currentMask = document.getElementById(`${appState.currentLanguage}Mask`);
  if (currentMask) {
    currentMask.setAttribute('mask', 'url(#maskClose)');
  }
  
  appState.currentLanguage = lang;
  
  const newMask = document.getElementById(`${lang}Mask`);
  if (newMask) {
    newMask.setAttribute('mask', 'url(#maskOpen)');
  }
  
  // Update UI text based on language
  // This would implement proper internationalization
}

/**
 * Handle window resize
 */
function handleWindowResize() {
  const width = window.innerWidth;
  
  // Adjust UI based on screen size
  if (width < 768) {
    // Mobile layout
  } else if (width < 1200) {
    // Tablet layout
  } else {
    // Desktop layout
  }
}

/**
 * Set up SVG filters for non-Chrome browsers
 */
function setupSvgFilters() {
  const isChromeOrEdge = /Chrome|Edge/.test(navigator.userAgent);
  
  if (!isChromeOrEdge) {
    const diffuseLighting = document.querySelectorAll('feDiffuseLighting');
    diffuseLighting.forEach(element => {
      const surfaceScale = element.getAttribute('surfaceScale');
      element.setAttribute('surfaceScale', parseFloat(surfaceScale) * 3);
      element.children[0].setAttribute('elevation', 30);
    });
    
    const specularLighting = document.querySelectorAll('feSpecularLighting');
    specularLighting.forEach(element => {
      const surfaceScale = element.getAttribute('surfaceScale');
      element.setAttribute('surfaceScale', parseFloat(surfaceScale) * 3);
      element.setAttribute('specularExponent', 50);
      element.children[0].setAttribute('elevation', 30);
    });
  }
}

/**
 * Full screen image view
 * @param {string} imageSrc - Image source URL
 */
function fullscreenImage(imageSrc) {
  const fsElement = document.getElementById('fsPic');
  if (!fsElement) return;
  
  const img = new Image();
  img.onload = function() {
    fsElement.style.display = 'block';
    document.body.style.overflow = 'hidden';
    
    // Center image
    if (this.width / window.innerWidth > this.height / window.innerHeight) {
      this.style.marginTop = (window.innerHeight - this.height * window.innerWidth / this.width) / 2 + 'px';
      this.width = window.innerWidth;
    } else {
      this.style.marginLeft = (window.innerWidth - this.width * window.innerHeight / this.height) / 2 + 'px';
      this.height = window.innerHeight;
    }
    
    // Add click handler to close on second click
    this.addEventListener('click', () => {
      document.body.style.overflow = 'visible';
      fsElement.style.display = 'none';
      fsElement.innerHTML = '';
    });
    
    fsElement.appendChild(this);
  };
  
  img.src = imageSrc;
}

// Make helper functions globally available
window.searchItem = searchItem;
window.exportCsv = exportCsv;
window.logout = logout;
window.setLanguage = setLanguage;
window.fullscreenImage = fullscreenImage;

// Initialize the application when the DOM is loaded
document.addEventListener('DOMContentLoaded', initApp);