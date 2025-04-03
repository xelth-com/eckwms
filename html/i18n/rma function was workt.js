/**
 * Применяет переводы к элементу и его дочерним элементам и удаляет атрибуты перевода
 * @param {HTMLElement} element - Контейнер с элементами для перевода
 */
function manuallyTranslateElement(element) {
  // Пропускаем если используем язык по умолчанию
  if (!window.i18n || window.i18n.getCurrentLanguage() === 'en') {
    return;
  }
  
  // Обрабатываем элементы с data-i18n
  const elementsWithI18n = [element, ...element.querySelectorAll('[data-i18n]')];
  elementsWithI18n.forEach(el => {
    if (el.hasAttribute('data-i18n')) {
      const key = el.getAttribute('data-i18n');
      
      // Извлекаем опции из атрибута data-i18n-options
      let options = {};
      try {
        const optionsAttr = el.getAttribute('data-i18n-options');
        if (optionsAttr) {
          options = JSON.parse(optionsAttr);
        }
      } catch (parseError) {
        console.error(`Error parsing data-i18n-options for ${key}:`, parseError);
      }
      
      // Получаем перевод
      const translation = getTranslation(key.includes(':') ? key : 'rma:' + key, options);
      if (translation && translation !== key) {
        el.textContent = translation;
        
        // ВАЖНОЕ ИСПРАВЛЕНИЕ: удаляем атрибут data-i18n после перевода!
        el.removeAttribute('data-i18n');
        
        // Также удаляем data-i18n-options если он есть
        if (el.hasAttribute('data-i18n-options')) {
          el.removeAttribute('data-i18n-options');
        }
        
        console.log(`[RMA] Removed data-i18n after translation for "${key}"`);
      }
    }
  });
  
  // Обрабатываем атрибутные переводы (data-i18n-attr)
  const elementsWithAttrI18n = [element, ...element.querySelectorAll('[data-i18n-attr]')];
  elementsWithAttrI18n.forEach(el => {
    if (el.hasAttribute('data-i18n-attr')) {
      try {
        const attrsMap = JSON.parse(el.getAttribute('data-i18n-attr'));
        
        // Извлекаем опции для этого элемента
        let options = {};
        const optionsAttr = el.getAttribute('data-i18n-options');
        if (optionsAttr) {
          try {
            options = JSON.parse(optionsAttr);
          } catch (parseError) {
            console.error(`Error parsing data-i18n-options:`, parseError);
          }
        }
        
        // Счетчик для отслеживания успешных переводов
        let successfulTranslations = 0;
        const totalAttributes = Object.keys(attrsMap).length;
        
        // Обрабатываем каждый атрибут
        for (const [attr, key] of Object.entries(attrsMap)) {
          const translation = getTranslation(key.includes(':') ? key : 'rma:' + key, options);
          if (translation && translation !== key) {
            el.setAttribute(attr, translation);
            successfulTranslations++;
          }
        }
        
        // ВАЖНОЕ ИСПРАВЛЕНИЕ: удаляем data-i18n-attr после перевода всех атрибутов!
        if (successfulTranslations === totalAttributes) {
          el.removeAttribute('data-i18n-attr');
          
          // Также удаляем data-i18n-options если он есть
          if (el.hasAttribute('data-i18n-options')) {
            el.removeAttribute('data-i18n-options');
          }
          
          console.log(`[RMA] Removed data-i18n-attr after translating all ${totalAttributes} attributes`);
        }
      } catch (e) {
        console.error('Error parsing data-i18n-attr:', e);
      }
    }
  });
  
  // Обрабатываем HTML переводы (data-i18n-html)
  const elementsWithHtmlI18n = [element, ...element.querySelectorAll('[data-i18n-html]')];
  elementsWithHtmlI18n.forEach(el => {
    if (el.hasAttribute('data-i18n-html')) {
      const key = el.getAttribute('data-i18n-html');
      
      // Извлекаем опции
      let options = {};
      const optionsAttr = el.getAttribute('data-i18n-options');
      if (optionsAttr) {
        try {
          options = JSON.parse(optionsAttr);
        } catch (parseError) {
          console.error(`Error parsing data-i18n-options for HTML ${key}:`, parseError);
        }
      }
      
      const translation = getTranslation(key.includes(':') ? key : 'rma:' + key, options);
      if (translation && translation !== key) {
        el.innerHTML = translation;
        
        // ВАЖНОЕ ИСПРАВЛЕНИЕ: удаляем data-i18n-html после перевода!
        el.removeAttribute('data-i18n-html');
        
        // Также удаляем data-i18n-options если он есть
        if (el.hasAttribute('data-i18n-options')) {
          el.removeAttribute('data-i18n-options');
        }
        
        console.log(`[RMA] Removed data-i18n-html after translation for "${key}"`);
      }
    }
  });
}