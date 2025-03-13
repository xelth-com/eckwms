import React from 'react';
import { createRoot } from 'react-dom/client';
import RMAForm from './components/RMAForm';

// Ждем, пока DOM полностью загрузится
document.addEventListener('DOMContentLoaded', () => {
  // Найти контейнер для формы
  const container = document.getElementById('rma-form-container');
  if (container) {
    // Создать корень React
    const root = createRoot(container);
    // Рендерить форму с обработчиком для кнопки "Back"
    root.render(
      <RMAForm 
        onBackClick={() => {
          // Возврат на предыдущую страницу через myFetch
          myFetch('', 'startInput', 'output2');
        }} 
      />
    );
  }
});