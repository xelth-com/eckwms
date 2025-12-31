# 🚀 ШПАРГАЛКА ДЛЯ CLAUDE - ECKWMS Production Server

> **ПРОЧИТАЙ ЭТО ПЕРВЫМ!** Это критическая информация для работы с продакшеном.
>
> ✅ **Безопасно для git** - нет секретных данных, только публичная информация

## ⚠️ ТЫ НА ЛОКАЛЬНОЙ МАШИНЕ!

Для работы с продакшеном используй SSH: `ssh root@xelth.com`

---

## 📋 Критические данные

```bash
SSH:      ssh root@xelth.com
Проект:   /var/www/eckwms/
PM2:      eckwms-global  (НЕ eckwms!)
Порт:     8080
БД:       PostgreSQL (eckwms_global) на localhost:5432
.env:     /var/www/eckwms/services/eckwms-global/.env
```

---

## 🔥 Частые команды (копируй и используй)

### 1. Проверить статус сервиса
```bash
ssh root@xelth.com "pm2 status eckwms-global"
```

### 2. Перезапустить и посмотреть логи
```bash
ssh root@xelth.com "pm2 restart eckwms-global && pm2 logs eckwms-global --lines 20 --nostream"
```

### 3. Запустить скрипт на сервере
```bash
ssh root@xelth.com "cd /var/www/eckwms && node services/eckwms-global/scripts/your-script.js"
```

### 4. Обновить код с GitHub
```bash
ssh root@xelth.com "cd /var/www/eckwms && git pull && pm2 restart eckwms-global"
```

### 5. Посмотреть логи в реальном времени
```bash
ssh root@xelth.com "pm2 logs eckwms-global"
```

---

## 🗄️ База данных PostgreSQL

### Подключиться к БД
```bash
ssh root@xelth.com "psql -U postgres -d eckwms_global"
```

### Проверить таблицы
```sql
\dt                        -- показать таблицы
\d registered_devices      -- описание таблицы
SELECT * FROM registered_devices LIMIT 5;
\q                         -- выйти
```

### Бэкап БД
```bash
ssh root@xelth.com "pg_dump -U postgres eckwms_global | gzip > /var/www/eckwms_backup_\$(date +%Y-%m-%d_%H-%M).sql.gz"
```

---

## 📝 Скрипты миграций БД

### ⚠️ ВАЖНО: Правильный шаблон скрипта

```javascript
// services/eckwms-global/scripts/your-migration.js
require('dotenv').config({ path: './services/eckwms-global/.env' });
const { Client } = require('pg');

async function run() {
  const client = new Client({
    host: process.env.PG_HOST || 'localhost',
    port: process.env.PG_PORT || 5432,
    database: process.env.PG_DATABASE || 'eckwms_global',
    user: process.env.PG_USERNAME,
    password: process.env.PG_PASSWORD
  });

  try {
    await client.connect();
    console.log('Connected to DB');

    // Твои SQL запросы здесь
    await client.query('YOUR SQL HERE');

    console.log('Migration complete');
  } catch (e) {
    console.error('Migration failed:', e);
    process.exit(1);
  } finally {
    await client.end();
  }
}

run();
```

### Запуск миграции
```bash
# 1. Создай скрипт локально в services/eckwms-global/scripts/
# 2. Коммит и push на GitHub
# 3. На сервере:
ssh root@xelth.com "cd /var/www/eckwms && git pull && node services/eckwms-global/scripts/your-migration.js && pm2 restart eckwms-global"
```

---

## 🔍 Диагностика проблем

### Сервис не запускается
```bash
ssh root@xelth.com "pm2 logs eckwms-global --err --lines 50"
```

### Проверить порт
```bash
ssh root@xelth.com "lsof -i :8080"
```

### Проверить БД
```bash
ssh root@xelth.com "psql -U postgres -c 'SELECT 1' eckwms_global"
```

---

## 📁 Структура проекта на сервере

```
/var/www/eckwms/
├── services/
│   └── eckwms-global/          ← ГЛАВНЫЙ МИКРОСЕРВИС
│       ├── .env                ← ОСНОВНОЙ .env ФАЙЛ
│       ├── src/
│       │   └── server.js       ← Точка входа (PM2 запускает это)
│       ├── scripts/            ← Миграции и утилиты
│       └── logs/               ← PM2 логи
├── QUICK_REFERENCE.md          ← ЭТОТ ФАЙЛ (в корне проекта)
├── .eck/
│   └── SERVER_ACCESS.md        ← Детальная документация
└── .git/
```

---

## ⚡ Workflow для изменений

### На локальной машине:
```bash
# 1. Создай/измени код
# 2. Коммит
git add .
git commit -m "описание изменений"
git push origin main
```

### На сервере (автоматически через SSH):
```bash
ssh root@xelth.com "cd /var/www/eckwms && git pull && pm2 restart eckwms-global && pm2 logs eckwms-global --lines 20 --nostream"
```

---

## 🚨 Критические ошибки, которых надо избегать

❌ **НЕ ДЕЛАЙ:**
- `pm2 restart eckwms` (неправильное имя!)
- `require('dotenv').config({ path: '../../.env' })` (неправильный путь!)
- Запускать скрипты миграций без SSH на локальной машине
- Использовать MySQL вместо PostgreSQL для eckwms-global

✅ **ДЕЛАЙ:**
- `pm2 restart eckwms-global`
- `require('dotenv').config({ path: './services/eckwms-global/.env' })`
- Всегда используй SSH для работы с продакшеном
- PostgreSQL для eckwms-global

---

## 📚 Дополнительная документация

- **Этот файл:** `QUICK_REFERENCE.md` (в корне проекта, виден всем)
- **Детали сервера:** `.eck/SERVER_ACCESS.md` (конфиденциально, не в git)
- **Workflow:** `.eck/REMOTE_DEVELOPMENT.md`

---

**Последнее обновление:** 2025-12-31
**Сервер:** xelth.com (Antigravity)
**PM2 Service:** eckwms-global
**Database:** PostgreSQL (eckwms_global)
