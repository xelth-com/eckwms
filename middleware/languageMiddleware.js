// middleware/languageMiddleware.js
/**
 * Middleware для обработки языковых настроек в запросах и ответах
 * Устанавливает язык в заголовках и добавляет мета-тег в HTML ответы
 */

/**
 * Создает middleware для работы с языками
 * @returns {Function} Express middleware
 */
function createLanguageMiddleware() {
  return (req, res, next) => {
    // Получаем язык из разных источников с приоритетом
    const language = req.query.lang ||               // URL параметр имеет высший приоритет
                    req.i18n?.language ||           // Затем язык, определенный i18n
                    process.env.DEFAULT_LANGUAGE || // Затем язык из переменных окружения
                    'en';                           // Дефолтный язык как последний вариант

    console.log(`[i18n] Setting language in response: ${language} for path ${req.path}`);
    
    try {
      // Всегда устанавливаем заголовок независимо от языка
      if (!res.headersSent) {
        res.setHeader('app-language', language);

        // Перехватываем метод res.send для добавления мета-тега в HTML
        const originalSend = res.send;
        res.send = function(body) {
          if (typeof body === 'string' && 
              (res.get('Content-Type')?.includes('text/html') || 
              body.includes('<head>') || 
              body.includes('<!DOCTYPE html>'))) {
            
            // Модифицируем только HTML ответы
            const languageMeta = `<meta name="app-language" content="${language}">`;
            
            // Добавляем мета-тег внутрь head
            if (body.includes('<head>')) {
              body = body.replace('<head>', `<head>\n    ${languageMeta}`);
            }
          }
          
          return originalSend.call(this, body);
        };
      }
    } catch (error) {
      console.error("Failed to set language headers:", error);
    }
    
    next();
  };
}

module.exports = createLanguageMiddleware;