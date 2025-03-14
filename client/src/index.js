// client/src/index.js
import React from 'react';
import { createRoot } from 'react-dom/client';
import App from './App';
import { AuthProvider } from './components/AuthContext';

console.log('Bundle loaded');

// Initialize the app with Auth Provider
window.initApp = function(containerId) {
  const container = document.getElementById(containerId);
  if (!container) {
    console.error(`Container #${containerId} not found`);
    return;
  }
  
  console.log(`Initializing app in #${containerId}`);
  
  try {
    const root = createRoot(container);
    root.render(
      <AuthProvider>
        <App />
      </AuthProvider>
    );
    console.log('App rendered successfully');
  } catch (err) {
    console.error('Error rendering app:', err);
    container.innerHTML = `
      <div style="color: red; padding: 20px;">
        Error loading application. Please try again.
      </div>
    `;
  }
};

// RMA form initialization function
window.initRMAForm = function(containerId, initialRmaCode) {
  const container = document.getElementById(containerId);
  if (!container) {
    console.error(`Container #${containerId} not found`);
    return;
  }
  
  const RMAForm = require('./components/RMAForm').default;
  
  try {
    container.innerHTML = '';
    
    const root = createRoot(container);
    root.render(
      <AuthProvider>
        <RMAForm 
          initialRmaCode={initialRmaCode || ''}
          userData={window.RMA_DATA || {}}
          onBackClick={() => {
            return window.myFetch ? window.myFetch('', 'startInput', 'output2') : window.history.back();
          }} 
        />
      </AuthProvider>
    );
    console.log('RMA form rendered successfully');
  } catch (err) {
    console.error('Error rendering RMA form:', err);
    container.innerHTML = `
      <div style="color: red; padding: 20px;">
        Error loading RMA form. Please try again.
      </div>
    `;
  }
};

// Auto-initialize if containers exist
document.addEventListener('DOMContentLoaded', () => {
  // Check for app container
  const appContainer = document.getElementById('app-container');
  if (appContainer) {
    window.initApp('app-container');
  }
  
  // Check for RMA form container
  const rmaContainer = document.getElementById('rma-form-container');
  if (rmaContainer) {
    const rmaCode = rmaContainer.getAttribute('data-rma-code');
    window.initRMAForm('rma-form-container', rmaCode);
  }
});