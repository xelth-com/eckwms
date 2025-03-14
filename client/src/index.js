import React from 'react';
import { createRoot } from 'react-dom/client';
import RMAForm from './components/RMAForm';

console.log('RMA bundle loaded');

// Глобальная функция для инициализации RMA формы в указанном контейнере
window.initRMAForm = function(containerId, initialRmaCode) {
  const container = document.getElementById(containerId);
  if (!container) {
    console.error(`Контейнер #${containerId} не найден`);
    return;
  }
  
  console.log(`Инициализация RMA формы в #${containerId} с кодом ${initialRmaCode}`);
  
  try {
    // Очищаем контейнер перед инициализацией
    container.innerHTML = '';
    
    const root = createRoot(container);
    root.render(
      <RMAForm 
        initialRmaCode={initialRmaCode || ''}
        onBackClick={() => {
          // Возврат на предыдущую страницу через myFetch
          return myFetch('', 'startInput', 'output2');
        }} 
      />
    );
    console.log('RMA форма успешно отрендерена');
  } catch (err) {
    console.error('Ошибка рендеринга RMA формы:', err);
    container.innerHTML = `
      <div class="cellPaper" style="color: red; padding: 20px;">
        Ошибка загрузки формы. Пожалуйста, попробуйте еще раз.
      </div>
    `;
  }
};

// Автоматическая инициализация, если контейнер существует при загрузке скрипта
console.log('Проверка наличия контейнера RMA формы');
const container = document.getElementById('rma-form-container');
if (container) {
  console.log('Найден контейнер RMA, инициализация');
  const rmaCode = container.getAttribute('data-rma-code');
  window.initRMAForm('rma-form-container', rmaCode);
}