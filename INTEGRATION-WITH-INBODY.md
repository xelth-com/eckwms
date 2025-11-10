# eckWMS ↔ InBody Service Center Integration

## 🎯 Архитектура интеграции

Проект разделен на **две части** с использованием **одной общей базы данных**:

### 1. **WMS Core** (универсальная складская система)
Чистый WMS для управления складом, заказами, сканированием и устройствами.

**Таблицы:**
- `scans` - универсальный буфер сканов с мобильных устройств
- `eckwms_instances` - мультитенантные инстансы WMS
- `registered_devices` - зарегистрированные мобильные устройства
- `user_auths` - пользователи системы
- `translation_caches` - кеш переводов

### 2. **InBody Driver** (специфичная бизнес-логика для InBody)
Специализированные таблицы для сервисного центра InBody.

**Таблицы:**
- `repair_orders` - ремонтные заказы (связаны со `scans` через `scan_id`)
- `repair_defective_parts` - дефектные части
- `repair_firmware_history` - история обновлений прошивки
- `repair_documents` - документы
- `support_cases` - AI поддержка клиентов
- `email_archive` - архив переписки

---

## 🔗 Ключевые связи

```
┌─────────────────┐
│  WMS Core       │
│  (Universal)    │
├─────────────────┤
│ eckwms_instances│◄────┐
│ scans           │     │ Foreign Key
│ registered_     │     │
│   devices       │     │
└─────────────────┘     │
                        │
                        │
┌─────────────────┐     │
│ InBody Driver   │     │
│ (Specific)      │     │
├─────────────────┤     │
│ repair_orders   │─────┘
│   ├─ scan_id (UUID)
│ repair_defective│
│   _parts        │
│ support_cases   │
└─────────────────┘
```

**Связь:** `repair_orders.scan_id` → `scans.id` (UUID)

---

## 📊 База данных

**Название:** `inbody_ai_support`
**Пользователь:** `inbody_user`
**Хост:** `localhost:5432`

### Конфигурация (.env)
```env
PG_DATABASE=inbody_ai_support
PG_USERNAME=inbody_user
PG_PASSWORD=beliberdabeliberden
PG_HOST=localhost
PG_PORT=5432
```

---

## 🚀 Установка и настройка

### 1. Запустить миграции

```bash
cd /mnt/c/Users/Dmytro/eckwms

# Запустить миграцию интеграции
PGPASSWORD=beliberdabeliberden psql -U inbody_user -d inbody_ai_support -h localhost \
  -f migrations/002-fix-table-creation-order.sql
```

### 2. Проверить подключение

```bash
node test-db-connection.js
```

**Ожидаемый результат:**
```
✅ ALL TESTS PASSED!
📊 Summary:
   • Database: inbody_ai_support
   • Scans: 346
   • Instances: 1
   • Repair Orders: 18
   • Linked Orders: 0

🎉 eckWMS is successfully integrated with InBody database!
```

### 3. Запустить eckWMS

```bash
# Локальный сервер
npm run dev:local

# Глобальный сервер
npm run dev:global
```

---

## 🔌 Использование в коде

### Доступ к моделям

```javascript
const db = require('./src/shared/models/postgresql');

// WMS Core models
await db.Scan.findAll();
await db.EckwmsInstance.findAll();
await db.RegisteredDevice.findAll();

// InBody Driver models
await db.RepairOrder.findAll();

// Связанные запросы
const repairOrder = await db.RepairOrder.findOne({
  include: [{
    model: db.Scan,
    as: 'scan'
  }]
});
```

### Создание связи между сканом и ремонтным заказом

```javascript
// Способ 1: SQL функция
await db.sequelize.query(
  'SELECT link_scan_to_repair_order($1, $2)',
  {
    bind: [scanId, repairOrderId],
    type: db.Sequelize.QueryTypes.SELECT
  }
);

// Способ 2: Sequelize ORM
await db.RepairOrder.update(
  { scan_id: scanId },
  { where: { id: repairOrderId } }
);
```

### Получить ремонтный заказ по скану

```javascript
// SQL функция
const result = await db.sequelize.query(
  'SELECT * FROM get_repair_order_from_scan($1)',
  {
    bind: [scanId],
    type: db.Sequelize.QueryTypes.SELECT
  }
);

// Sequelize
const repairOrders = await db.RepairOrder.findAll({
  where: { scan_id: scanId }
});
```

---

## 📋 Полезные SQL запросы

### Просмотр интегрированных данных

```sql
-- Все сканы с привязанными ремонтными заказами
SELECT * FROM v_scans_with_repairs LIMIT 10;

-- Статистика по сканам
SELECT
  status,
  COUNT(*) as count
FROM scans
GROUP BY status;

-- Ремонтные заказы без сканов
SELECT
  order_number,
  customer_name,
  device_model
FROM repair_orders
WHERE scan_id IS NULL;

-- Последние сканы от устройства
SELECT * FROM scans
WHERE "deviceId" = 'your_device_id'
ORDER BY "createdAt" DESC
LIMIT 10;
```

---

## 🗂️ Структура проекта

```
eckwms/
├── src/
│   └── shared/
│       └── models/
│           └── postgresql/
│               ├── index.js              # Инициализация моделей и связей
│               ├── Scan.js               # WMS: Сканы
│               ├── EckwmsInstance.js     # WMS: Инстансы
│               ├── RegisteredDevice.js   # WMS: Устройства
│               ├── UserAuth.js           # WMS: Пользователи
│               └── RepairOrder.js        # InBody: Ремонтные заказы
│
├── migrations/
│   ├── 001-integrate-with-inbody.sql    # Первая миграция (с ошибкой)
│   └── 002-fix-table-creation-order.sql # Рабочая миграция ✅
│
├── test-db-connection.js                # Тест подключения
├── .env                                 # Конфигурация (обновлена)
└── INTEGRATION-WITH-INBODY.md          # Эта документация
```

---

## 🎨 Преимущества архитектуры

### ✅ Разделение ответственности
- **WMS Core:** Универсальная логика, переиспользуемая
- **InBody Driver:** Специфичные таблицы только для InBody

### ✅ Единая точка истины
- Все данные в одной базе
- Нет проблем с синхронизацией
- Атомарные транзакции

### ✅ Гибкость
- Легко добавить новые "драйверы" для других клиентов
- WMS можно использовать отдельно
- InBody-специфичные таблицы не влияют на WMS

### ✅ Масштабируемость
- Мультитенантность через `eckwms_instances`
- Каждый клиент может иметь свой инстанс
- Или использовать standalone (как InBody)

---

## 🔍 Интеграционные точки

### 1. Сканирование → Ремонтный заказ
```javascript
// Когда приходит скан с устройства
const scan = await db.Scan.create({
  deviceId: 'device123',
  payload: 'I10301825',
  type: 'Code128',
  status: 'buffered'
});

// Если это серийный номер устройства InBody
const repairOrder = await db.RepairOrder.create({
  order_number: 'CS-DE-251107-001',
  device_serial: scan.payload,
  scan_id: scan.id, // 🔗 Связь!
  // ... другие поля
});
```

### 2. Просмотр истории устройства
```javascript
// Получить все сканы устройства
const scans = await db.Scan.findAll({
  where: { deviceId: 'device123' },
  include: [{
    model: db.RepairOrder,
    as: 'repairOrders'
  }],
  order: [['createdAt', 'DESC']]
});
```

### 3. Статистика и отчеты
```javascript
// Используем готовую view
const stats = await db.sequelize.query(
  `SELECT
    COUNT(DISTINCT scan_id) as scanned_items,
    COUNT(DISTINCT repair_order_id) as repair_orders,
    COUNT(*) as total_scans
   FROM v_scans_with_repairs
   WHERE scan_created_at >= NOW() - INTERVAL '30 days'`,
  { type: db.Sequelize.QueryTypes.SELECT }
);
```

---

## 🛠️ Команды для разработки

```bash
# Тест подключения к БД
node test-db-connection.js

# Запустить локальный сервер (разработка)
npm run dev:local

# Запустить глобальный сервер (разработка)
npm run dev:global

# Запустить в продакшн
npm run start:local
npm run start:global

# Проверить структуру БД
PGPASSWORD=beliberdabeliberden psql -U inbody_user -d inbody_ai_support -h localhost -c "\dt"

# Просмотреть последние сканы
PGPASSWORD=beliberdabeliberden psql -U inbody_user -d inbody_ai_support -h localhost \
  -c "SELECT * FROM v_scans_with_repairs LIMIT 5;"
```

---

## 📝 Заметки

### Миграция данных
Старая таблица `scans` использовала `SERIAL id`, новая использует `UUID id`.
Данные были автоматически мигрированы с генерацией новых UUID.
Оригинальные данные сохранены в `scans_backup`.

### InBody инстанс
Автоматически создан специальный инстанс для InBody:
- **ID:** `00000000-0000-0000-0000-000000000001`
- **Name:** `InBody Service Center`
- **Tier:** `paid`

Этот инстанс можно использовать для standalone режима (без multi-tenancy).

### Sequelize logging
Для продакшена отключите логирование в `src/shared/models/postgresql/index.js`:
```javascript
logging: false  // Вместо logging: process.env.NODE_ENV !== 'production'
```

---

## 📞 Поддержка

Если возникли проблемы:
1. Проверьте `.env` файл
2. Запустите `node test-db-connection.js`
3. Проверьте логи PostgreSQL
4. Убедитесь, что база данных запущена

---

**Дата создания:** 2025-11-10
**Версия:** 1.0.0
**Статус:** ✅ Работает и протестировано
