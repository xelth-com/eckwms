# Технологический стек ECKWMS

## Backend

### Runtime & Framework
- **Node.js** - JavaScript runtime
- **Express.js** - Web framework для REST API
- **Sequelize** - ORM для работы с базами данных

### База данных

**Development:**
- **SQLite** - Файловая БД для локальной разработки

**Production:**
- **PostgreSQL** - Реляционная БД для продакшена
- **pg** (node-postgres) - PostgreSQL клиент для Node.js

### Process Management
- **PM2** - Process manager для Node.js приложений (только production)
  - Автоматический рестарт
  - Логирование
  - Мониторинг

### Инфраструктура (Production)
- **Nginx** - Reverse proxy и load balancer
- **Linux (Ubuntu)** - Операционная система сервера
- **systemd** - Управление системными сервисами

---

## Утилиты и инструменты

### Package Management
- **npm** - Node package manager
- **package.json** - Управление зависимостями

### Version Control
- **Git** - Система контроля версий
- **GitHub** - Хостинг репозитория

### Environment Configuration
- **dotenv** - Управление переменными окружения
- **.env файлы** - Конфигурация для разных окружений

---

## Development Tools

### Code Quality
- **ESLint** (опционально) - Линтер для JavaScript
- **Prettier** (опционально) - Форматтер кода

### Testing
- **Jest** (опционально) - Testing framework
- **Supertest** (опционально) - HTTP assertions

---

## API & Communication

### Protocols
- **HTTP/HTTPS** - Основной протокол API
- **REST** - Архитектурный стиль API

### Data Format
- **JSON** - Формат обмена данными

---

## Security

### Authentication & Authorization
- **API Keys** - Простая аутентификация
- **HTTPS** - Шифрованное соединение (production)

### Data Validation
- **Sequelize validators** - Валидация данных
- **Express middleware** - Обработка ошибок

---

## Monitoring & Logging

### Application Logs
- **PM2 logs** - Логи процессов
- **Console logging** - Логи приложения

### Health Checks
- **Health endpoints** - Проверка работоспособности сервиса

---

## Dependencies (ключевые)

```json
{
  "express": "^4.x",
  "sequelize": "^6.x",
  "pg": "^8.x",
  "dotenv": "^16.x"
}
```

---

## Architecture Patterns

### Микросервисы
- Изолированные сервисы с собственными БД
- HTTP API для коммуникации

### MVC (частично)
- **Models** - Sequelize модели
- **Routes** - Express роуты
- **Controllers** - Бизнес-логика

### Прокси-паттерн
- Проксирование запросов между инстансами
- Централизованный роутинг

---

## Deployment

### Development
```bash
npm install
npm run dev
```

### Production
```bash
npm install --production
pm2 start ecosystem.config.js
```

---

## Будущие технологии (планируется)

См. [ROADMAP.md](ROADMAP.md) для планов по расширению стека.
