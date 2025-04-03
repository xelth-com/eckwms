# Руководство по интеграции обновлённой системы i18n

## 1. Обновление файла i18n.js

Добавьте новую функцию `translateElement` и обновите публичный API системы i18n:

1. Откройте файл `html/js/i18n.js`
2. Добавьте функцию `translateElement` перед определением публичного API
3. Замените определение `window.i18n` на обновлённую версию
4. Обновите функцию `setupMutationObserver`, чтобы она использовала новый метод

## 2. Упрощение RMA скрипта

Модифицируйте `rma-form.js`, чтобы он использовал новый метод вместо собственного:

```javascript
// Замените функцию manuallyTranslateElement на:
function manuallyTranslateElement(element) {
  if (window.i18n && typeof window.i18n.translateElement === 'function') {
    window.i18n.translateElement(element);
  } else {
    console.warn('i18n.translateElement is not available, falling back to local implementation');
    // Здесь можно оставить существующую реализацию как запасной вариант
  }
}
```

## 3. Для новых страниц и форм

Для перевода элементов на новых страницах или формах:

```javascript
// Перевод всего контейнера формы
const formContainer = document.getElementById('myForm');
if (window.i18n) {
  window.i18n.translateElement(formContainer);
}

// Перевод динамически добавленных элементов
function addNewFormField() {
  const newField = document.createElement('div');
  newField.innerHTML = `
    <label data-i18n="form.field_name">Field Name</label>
    <input type="text" data-i18n-attr='{"placeholder":"form.enter_name"}'>
  `;
  formContainer.appendChild(newField);
  
  // Перевод нового элемента
  if (window.i18n) {
    window.i18n.translateElement(newField);
  }
}
```

## 4. Преимущества нового подхода

- **Единый код**: Общая логика перевода находится в одном месте
- **Автоматическое удаление атрибутов**: Атрибуты удаляются после успешного перевода
- **Совместимость**: Работает с существующими i18n-атрибутами
- **Простота**: Один вызов метода вместо сложной логики на каждой странице

## 5. Примеры использования

### Кнопка изменения языка:
```javascript
document.getElementById('changeLanguageBtn').addEventListener('click', function() {
  // Изменить язык
  window.i18n.changeLanguage('de');
});
```

### Перевод динамического контента:
```javascript
async function loadAndTranslateContent() {
  const response = await fetch('/api/content');
  const data = await response.json();
  
  const container = document.getElementById('dynamicContent');
  container.innerHTML = `
    <h2 data-i18n="content.title">${data.title}</h2>
    <p data-i18n="content.description">${data.description}</p>
  `;
  
  // Перевести весь добавленный контент
  window.i18n.translateElement(container);
}
```