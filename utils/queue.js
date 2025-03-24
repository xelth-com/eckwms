// utils/queue.js - Улучшенная обработка очереди
class Queue {
  constructor() {
    this.items = [];
    this.processingMap = new Map(); // Отслеживание обрабатываемых элементов
    this.processedKeys = new Set(); // Для отслеживания уже добавленных ключей
    this.priorityScores = new Map(); // Для приоритизации элементов
  }
  
  // Добавить элемент в очередь с умной приоритизацией
  enqueue(item) {
    const uniqueKey = `${item.targetLang}:${item.namespace}:${item.key}`;
    
    // Проверяем, не обрабатывается ли уже этот ключ
    if (this.processingMap.has(uniqueKey)) {
      return false;
    }
    
    // Проверяем, не был ли этот ключ уже добавлен
    if (!this.processedKeys.has(uniqueKey)) {
      // Рассчитываем приоритет (например, на основе длины текста и числа использований)
      const priority = this.calculatePriority(item);
      item.priority = priority;
      
      // Вставляем элемент с учетом приоритета
      const insertIndex = this.findInsertIndex(priority);
      this.items.splice(insertIndex, 0, item);
      
      this.processedKeys.add(uniqueKey);
      return true;
    }
    
    return false;
  }
  
  // Вычисление приоритета для элемента
  calculatePriority(item) {
    // Базовый приоритет
    let score = 50;
    
    // Короткие тексты получают более высокий приоритет (быстрее переводятся)
    if (item.text.length < 50) score += 20;
    else if (item.text.length > 500) score -= 20;
    
    // Популярные языки получают более высокий приоритет
    const popularLanguages = ['en', 'de', 'fr', 'es', 'it', 'ru', 'zh', 'ja', 'ko'];
    if (popularLanguages.includes(item.targetLang)) score += 10;
    
    // Учитываем предыдущие запросы на этот ключ
    const keyBase = `${item.targetLang}:${item.namespace}`;
    const prevScore = this.priorityScores.get(keyBase) || 0;
    score += prevScore;
    
    // Обновляем счетчик запросов для этого базового ключа
    this.priorityScores.set(keyBase, prevScore + 5);
    
    return score;
  }
  
  // Находим индекс для вставки на основе приоритета
  findInsertIndex(priority) {
    for (let i = 0; i < this.items.length; i++) {
      if (this.items[i].priority < priority) {
        return i;
      }
    }
    return this.items.length;
  }
  
  // Извлечь элемент из очереди с отметкой о начале обработки
  dequeue() {
    if (this.isEmpty()) {
      return null;
    }
    
    const item = this.items.shift();
    const uniqueKey = `${item.targetLang}:${item.namespace}:${item.key}`;
    
    // Отмечаем, что элемент в обработке
    this.processingMap.set(uniqueKey, {
      startTime: Date.now(),
      item
    });
    
    return item;
  }
  
  // Отметить завершение обработки элемента
  markProcessed(item, success = true) {
    const uniqueKey = `${item.targetLang}:${item.namespace}:${item.key}`;
    
    this.processingMap.delete(uniqueKey);
    
    // Если неудача, можно решить, нужно ли возвращать в очередь
    if (!success && item.retryCount < MAX_RETRIES) {
      // Увеличиваем счетчик попыток и снижаем приоритет
      item.retryCount = (item.retryCount || 0) + 1;
      item.priority = Math.max(1, item.priority - 20); // Снижаем приоритет
      
      // Возвращаем в очередь с задержкой
      setTimeout(() => {
        this.processedKeys.delete(uniqueKey); // Позволяем повторно добавить
        this.enqueue(item);
      }, RETRY_DELAYS[item.retryCount - 1] || 30000);
    }
  }
  
  // Получить информацию о текущем состоянии очереди
  getStats() {
    return {
      queuedItems: this.items.length,
      processingItems: this.processingMap.size,
      processedKeys: this.processedKeys.size,
      priorityKeys: this.priorityScores.size
    };
  }
}