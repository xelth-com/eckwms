# Руководство по Тестированию Системы Подключения Устройств

## Что было исправлено

### Проблемы, которые были найдены и исправлены:

1. **Модель RegisteredDevice** - поле `instance_id` было обязательным, но не предоставлялось при регистрации
   - ✅ Исправлено: поле `instance_id` теперь необязательное (`allowNull: true`)

2. **setup.js** - использовались неправильные имена полей (`isActive` вместо `is_active`)
   - ✅ Исправлено: обновлены имена полей для соответствия модели

3. **Глобальный сервер** - отсутствовал эндпоинт `/health`
   - ✅ Исправлено: добавлен эндпоинт `/health` для проверки статуса

4. **Файлы системы**
   - ✅ Создан: `src/server/local/views/admin/pairing.html`
   - ✅ Создан: `public/js/admin_pairing.js`
   - ✅ Обновлен: `src/server/local/routes/admin.js` (добавлен роут `/pairing`)
   - ✅ Обновлен: `src/server/local/routes/setup.js` (добавлен эндпоинт `/global-server-status`)

## Быстрая Проверка

Для быстрой проверки всех компонентов используйте тестовый скрипт:

```bash
bash scripts/test-pairing-system.sh
```

## Пошаговое Тестирование

### Шаг 1: Подготовка Окружения

1. **Проверьте .env файл**
   ```bash
   cat .env | grep -E "SERVER_PUBLIC_KEY|SERVER_PRIVATE_KEY|JWT_SECRET|GLOBAL_SERVER_URL"
   ```

   Должны быть установлены:
   - ✅ `SERVER_PUBLIC_KEY`
   - ✅ `SERVER_PRIVATE_KEY`
   - ✅ `JWT_SECRET`
   - ✅ `GLOBAL_SERVER_URL=http://localhost:8080`

2. **Проверьте PostgreSQL**
   ```bash
   nc -zv 172.29.16.1 5432
   ```
   Должно вернуть: `Connection to 172.29.16.1 5432 port [tcp/postgresql] succeeded!`

### Шаг 2: Запуск Серверов

**Терминал 1** - Локальный сервер:
```bash
npm run start:local
```

Ожидаемый вывод:
```
PostgreSQL connection established successfully.
PostgreSQL models synchronized.
eckwms server running on port 3000 in development mode.
```

**Терминал 2** - Глобальный сервер:
```bash
npm run start:global
```

Ожидаемый вывод:
```
========================================
eckWMS Global Server
========================================
Running on port: 8080
Proxying API requests to: http://localhost:3000
========================================
```

### Шаг 3: Вход в Систему

1. Откройте браузер и перейдите на:
   ```
   http://localhost:3000/auth/login
   ```

2. Войдите используя учетные данные администратора

3. После успешного входа JWT токен будет сохранен в `localStorage` браузера

### Шаг 4: Тестирование Страницы Подключения

1. Перейдите на страницу подключения устройств:
   ```
   http://localhost:3000/admin/pairing
   ```

2. **Проверка статуса глобального сервера**
   - На странице должна быть секция "System Status"
   - Статус должен показывать: `Global Server is ONLINE.` (зеленый фон)
   - Статус обновляется автоматически каждые 15 секунд

3. **Генерация QR кода**
   - Нажмите кнопку "Generate New QR Code"
   - В течение секунды должен появиться QR код
   - QR код содержит информацию для подключения устройства:
     ```json
     {
       "type": "eckwms-pairing-request",
       "version": "1.0",
       "local_server_url": "http://localhost:3000",
       "global_server_url": "http://localhost:8080",
       "server_public_key": "..."
     }
     ```

### Шаг 5: Проверка Эндпоинтов (опционально)

Вы можете проверить эндпоинты напрямую:

**Проверка здоровья глобального сервера:**
```bash
curl http://localhost:8080/health
```
Ответ: `{"status":"ok","timestamp":"2025-..."}`

**Проверка статуса через локальный сервер:**
```bash
curl -H "Authorization: Bearer YOUR_JWT_TOKEN" http://localhost:3000/api/internal/global-server-status
```
Ответ: `{"status":"online"}`

**Генерация QR кода через API:**
```bash
curl -H "Authorization: Bearer YOUR_JWT_TOKEN" http://localhost:3000/api/internal/pairing-qr
```
Ответ: `{"success":true,"qr_code_data_url":"data:image/png;base64,..."}`

### Шаг 6: Тестирование с Android Устройством

1. **Убедитесь, что компьютер и Android устройство в одной Wi-Fi сети**

2. **Узнайте локальный IP-адрес компьютера:**
   ```bash
   ipconfig  # для Windows
   ifconfig  # для Linux/Mac
   ```
   Например: `192.168.1.10`

3. **Обновите .env файл** (если тестируете с реального устройства):
   ```env
   LOCAL_SERVER_INTERNAL_URL=http://192.168.1.10:3000
   GLOBAL_SERVER_URL=http://192.168.1.10:8080
   ```

4. **Перезапустите серверы** после изменения .env

5. **Откройте приложение eckwms-movfast** на Android устройстве

6. **Нажмите "Connect to Server" → "Scan Pairing QR Code"**

7. **Отсканируйте QR код** с экрана компьютера

8. **Проверьте результат:**

   **На телефоне:**
   - Статус должен измениться на: `Success! Device paired with server.`

   **В логах локального сервера:**
   ```
   [Device Registration] New device registered: <DEVICE_ID>
   ```

9. **Проверьте базу данных:**
   ```bash
   # Если у вас установлен psql
   PGPASSWORD=gK76543n2PqX5bV9zR4m psql -h 172.29.16.1 -U wms_user -d eckwms \
     -c "SELECT \"deviceId\", \"deviceName\", \"is_active\" FROM registered_devices;"
   ```

## Тестирование Офлайн Режима

1. **Остановите глобальный сервер** (Ctrl+C в терминале 2)

2. **Подождите до 15 секунд** на странице подключения

3. **Проверьте изменение статуса:**
   - Должно появиться: `Global Server is OFFLINE.` (красный фон)
   - Сообщение об ошибке должно содержать детали

4. **Попробуйте сгенерировать QR код:**
   - QR код все равно должен генерироваться (использует локальный сервер)

5. **Запустите глобальный сервер снова:**
   ```bash
   npm run start:global
   ```

6. **Проверьте, что статус обновился на ONLINE**

## Отладка

### Проблема: QR код не генерируется

**Причина:** Нет JWT токена в localStorage
**Решение:**
- Убедитесь, что вы вошли в систему
- Проверьте консоль браузера (F12) на наличие ошибок
- Проверьте `localStorage.getItem('auth_token')` в консоли браузера

### Проблема: Статус показывает OFFLINE, хотя сервер запущен

**Причина:** Неправильный GLOBAL_SERVER_URL в .env
**Решение:**
- Проверьте .env файл: `GLOBAL_SERVER_URL=http://localhost:8080`
- Проверьте, что глобальный сервер действительно запущен на порту 8080
- Проверьте эндпоинт напрямую: `curl http://localhost:8080/health`

### Проблема: Ошибка при регистрации устройства

**Причина:** Проблема с базой данных
**Решение:**
- Проверьте подключение к PostgreSQL
- Проверьте логи локального сервера
- Убедитесь, что таблица `registered_devices` создана
- Проверьте, что модель синхронизирована: должно быть `DB_ALTER=false` в .env для продакшна

## Структура Файлов

```
eckwms/
├── src/
│   ├── server/
│   │   ├── local/
│   │   │   ├── routes/
│   │   │   │   ├── admin.js          # Добавлен роут /pairing
│   │   │   │   └── setup.js          # Добавлен /global-server-status
│   │   │   └── views/
│   │   │       └── admin/
│   │   │           └── pairing.html  # НОВЫЙ ФАЙЛ
│   │   └── global/
│   │       └── server.js             # Добавлен /health
│   └── shared/
│       └── models/
│           └── postgresql/
│               └── RegisteredDevice.js # Исправлен (instance_id nullable)
├── public/
│   └── js/
│       └── admin_pairing.js          # НОВЫЙ ФАЙЛ
├── scripts/
│   └── test-pairing-system.sh        # НОВЫЙ ФАЙЛ
└── docs/
    └── DEVICE_PAIRING_TEST_GUIDE.md  # Этот файл
```

## Следующие Шаги

После успешного тестирования вы можете:

1. Настроить production окружение
2. Реализовать офлайн-режим в Android приложении
3. Добавить кэширование данных
4. Настроить SSL сертификаты для HTTPS
5. Развернуть на реальном сервере

## Поддержка

Если возникли проблемы:
1. Запустите тестовый скрипт: `bash scripts/test-pairing-system.sh`
2. Проверьте логи серверов
3. Проверьте консоль браузера (F12)
4. Убедитесь, что все зависимости установлены: `npm install`
