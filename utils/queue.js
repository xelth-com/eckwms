// utils/queue.js

/**
 * Простая реализация очереди для обработки переводов
 */
class Queue {
  constructor() {
    this.items = [];
    this.processedKeys = new Set(); // Для отслеживания уже добавленных ключей
  }

  /**
   * Добавить элемент в очередь
   * @param {Object} item - Элемент для добавления
   */
  enqueue(item) {
    // Создаем уникальный ключ для элемента
    const uniqueKey = `${item.targetLang}:${item.namespace}:${item.key}`;
    
    // Проверяем, не был ли этот ключ уже добавлен
    if (!this.processedKeys.has(uniqueKey)) {
      this.items.push(item);
      this.processedKeys.add(uniqueKey);
      return true;
    }
    
    return false; // Элемент был уже добавлен
  }

  /**
   * Извлечь элемент из очереди
   * @returns {Object} Первый элемент в очереди
   */
  dequeue() {
    if (this.isEmpty()) {
      return null;
    }
    return this.items.shift();
  }

  /**
   * Проверить, пуста ли очередь
   * @returns {boolean} True, если очередь пуста
   */
  isEmpty() {
    return this.items.length === 0;
  }

  /**
   * Получить размер очереди
   * @returns {number} Количество элементов в очереди
   */
  size() {
    return this.items.length;
  }

  /**
   * Очистить очередь
   */
  clear() {
    this.items = [];
    this.processedKeys.clear();
  }
}

module.exports = { Queue };