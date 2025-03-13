import React from 'react';
import { createRoot } from 'react-dom/client';
import RMAForm from './components/RMAForm';

document.addEventListener('DOMContentLoaded', () => {
  const container = document.getElementById('rma-form-container');
  if (container) {
    const root = createRoot(container);
    root.render(<RMAForm />);
  }
});