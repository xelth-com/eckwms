/**
 * API Service for communicating with the backend
 * 
 * This service handles all HTTP requests to the server and provides
 * a clean interface for the frontend components.
 */
class ApiService {
  /**
   * Initialize the API service
   */
  constructor() {
    this.baseUrl = '/';
    this.authToken = localStorage.getItem('jwt');
  }

  /**
   * Set the authentication token
   * @param {string} token - JWT token
   */
  setAuthToken(token) {
    this.authToken = token;
    localStorage.setItem('jwt', token);
  }

  /**
   * Clear the authentication token
   */
  clearAuthToken() {
    this.authToken = null;
    localStorage.removeItem('jwt');
  }

  /**
   * Make a request to the API
   * @param {string} text - Request text/content
   * @param {string} name - Endpoint name
   * @param {string} destination - Response destination
   * @returns {Promise<any>} Response data
   */
  async makeRequest(text, name, destination) {
    try {
      const response = await fetch(this.baseUrl, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json'
        },
        body: JSON.stringify({
          text: text,
          name: name,
          dest: destination,
          jwt: this.authToken
        })
      });

      if (!response.ok) {
        throw new Error(`Network response was not ok: ${response.statusText}`);
      }

      // Handle different response types
      if (destination === 'pdfRma') {
        return await response.blob();
      } else if (destination === 'csv') {
        return await response.text();
      } else {
        return await response.text();
      }
    } catch (error) {
      console.error('API request failed:', error);
      throw error;
    }
  }

  /**
   * Submit a form
   * @param {HTMLFormElement} form - The form element
   * @param {string} formId - The form ID
   * @param {string} destination - Response destination
   * @returns {Promise<any>} Response data
   */
  async submitForm(form, formId, destination) {
    const formData = {};
    const elements = Array.from(form.querySelectorAll('input, textarea, select'));
    
    elements.forEach(element => {
      if (element.value) {
        formData[element.id] = element.value;
      }
    });

    return this.makeRequest(JSON.stringify(formData), formId, destination);
  }

  /**
   * Search for items by serial number or RMA number
   * @param {string} query - Search query
   * @returns {Promise<string>} Search results HTML
   */
  async search(query) {
    return this.makeRequest(query, 'snInput', 'output2');
  }

  /**
   * Fetch data for a specific entity
   * @param {string} id - Entity ID
   * @param {string} type - Entity type
   * @returns {Promise<string>} Entity data HTML
   */
  async fetchEntity(id, type) {
    return this.makeRequest(id, type, 'outputShow');
  }

  /**
   * Generate an RMA form
   * @returns {Promise<string>} RMA form HTML
   */
  async generateRmaForm() {
    return this.makeRequest('rmaGenerate', 'rmaButton', 'output2');
  }

  /**
   * Submit an RMA form
   * @param {Object} formData - Form data
   * @returns {Promise<Blob>} PDF blob
   */
  async submitRmaForm(formData) {
    const response = await this.makeRequest(JSON.stringify(formData), 'rmaForm', 'pdfRma');
    return response;
  }

  /**
   * Export data as CSV
   * @returns {Promise<string>} CSV data
   */
  async exportCsv() {
    return this.makeRequest('', 'logExp', 'csv');
  }

  /**
   * Load parts data for a specific model
   * @param {string} model - Model identifier
   * @returns {Promise<string>} Parts data HTML
   */
  async loadParts(model) {
    return this.makeRequest(`parts${model}`, `mainMenu${model}`, 'output1');
  }
}

// Create and export a singleton instance
const apiService = new ApiService();
export default apiService;