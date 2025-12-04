# eckWMS Global Server - PM2 Setup Guide

Это руководство для развертывания микросервиса eckWMS с использованием **PM2** (рекомендуемый метод для production).

## Предварительные требования

```bash
# Убедитесь, что PM2 установлен глобально
npm install -g pm2

# Проверьте версию
pm2 -v
```

## Установка и конфигурация

### 1. Установите зависимости микросервиса

```bash
cd /var/www/pda.repair/services/eckwms-global

# Установите зависимости
npm install
```

### 2. Создайте .env файл

```bash
# Скопируйте конфиг шаблон
cp .env.example .env

# Отредактируйте с вашими значениями
nano .env
```

**Важные переменные:**
```bash
PORT=8080
GLOBAL_SERVER_API_KEY=your_secure_api_key_here
PG_HOST=localhost
PG_DATABASE=eckwms
PG_USERNAME=wms_user
PG_PASSWORD=secure_password
ENC_KEY=your_encryption_key
```

### 3. Запустите с PM2

#### Опция A: Запуск отдельного микросервиса

```bash
# Запустить только eckwms-global
pm2 start src/server.js --name "eckwms-global" --env production

# Или с конфигом из root директории
cd /var/www/pda.repair
pm2 start ecosystem.config.js --only eckwms-global
```

#### Опция B: Запуск обоих сервисов вместе (рекомендуется)

```bash
cd /var/www/pda.repair

# Запустить оба сервиса (pda.repair + eckwms-global)
pm2 start ecosystem.config.js

# Или если уже запущены, перезагрузить
pm2 reload ecosystem.config.js
```

## Мониторинг и управление

### Проверить статус

```bash
# Список всех процессов
pm2 list

# Получит что-то вроде:
# ┌─────┬──────────────────┬─────────┬────────┐
# │ id  │ name             │ status  │ uptime │
# ├─────┼──────────────────┼─────────┼────────┤
# │ 8   │ pda.repair       │ online  │ 19D    │
# │ 9   │ eckwms-global    │ online  │ 1m     │
# └─────┴──────────────────┴─────────┴────────┘
```

### Просмотреть логи

```bash
# Логи всех сервисов
pm2 logs

# Только eckwms-global
pm2 logs eckwms-global

# Last 100 lines
pm2 logs eckwms-global --lines 100

# Real-time мониторинг
pm2 monit
```

### Управление процессом

```bash
# Перезагрузить микросервис (без downtime)
pm2 reload eckwms-global

# Перезапустить
pm2 restart eckwms-global

# Остановить
pm2 stop eckwms-global

# Запустить снова
pm2 start eckwms-global

# Удалить из PM2
pm2 delete eckwms-global
```

## Сохранение конфигурации

Сохраните текущую конфигурацию PM2, чтобы сервисы стартовали автоматически после перезагрузки:

```bash
# Сохранить текущее состояние
pm2 save

# Создать startup скрипт
pm2 startup

# Удалить из автостарта
pm2 unstartup
```

## Проверка здоровья сервиса

```bash
# Health check
curl http://localhost:8080/ECK/health

# Должен вернуть:
# {
#   "status": "healthy",
#   "service": "eckWMS Global Server",
#   "database": "connected",
#   "uptime": 123.45
# }
```

## Обновление после изменений

Если вы изменили код микросервиса:

```bash
cd /var/www/pda.repair/services/eckwms-global

# Обновить зависимости (если нужно)
npm install

# Перезагрузить сервис
pm2 reload eckwms-global

# Или просто перезапустить
pm2 restart eckwms-global
```

## Troubleshooting

### Микросервис не запускается

```bash
# Проверить логи
pm2 logs eckwms-global --lines 50

# Проверить конфиг .env
cat /var/www/pda.repair/services/eckwms-global/.env

# Проверить порт 8080
lsof -i :8080
```

### Ошибка подключения к БД

```bash
# Убедитесь, что PostgreSQL работает
psql -h localhost -U wms_user -d eckwms -c "SELECT version();"

# Проверьте переменные окружения
grep PG_ /var/www/pda.repair/services/eckwms-global/.env
```

### High memory usage

Если процесс потребляет много памяти:

```bash
# Перезагрузить (graceful restart)
pm2 reload eckwms-global

# Или установить лимит памяти в ecosystem.config.js
# max_memory_restart: '300M'
```

## Логирование

Логи находятся в `/var/www/pda.repair/logs/`:

```bash
# Последние 100 строк
tail -100 /var/www/pda.repair/logs/eckwms-global-out.log

# Ошибки
tail -50 /var/www/pda.repair/logs/eckwms-global-error.log

# Real-time логи
tail -f /var/www/pda.repair/logs/eckwms-global-combined.log
```

## Интеграция с Nginx

Убедитесь, что Nginx проксирует `/ECK/*` на `localhost:8080`:

```nginx
# /etc/nginx/sites-available/pda.repair

location /ECK {
    proxy_pass http://localhost:8080/ECK;
    proxy_set_header Host $host;
    proxy_set_header X-Real-IP $remote_addr;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    proxy_set_header X-Forwarded-Proto $scheme;
}
```

Перезагрузить Nginx:
```bash
sudo systemctl reload nginx
```

## Production Checklist

- [ ] Установлены все зависимости (`npm install`)
- [ ] Создан файл `.env` с production значениями
- [ ] PostgreSQL БД настроена
- [ ] Сервис запущен через PM2
- [ ] Настроено логирование
- [ ] Health check работает (`curl http://localhost:8080/ECK/health`)
- [ ] Nginx проксирует `/ECK/*` правильно
- [ ] Сохранена PM2 конфигурация (`pm2 save`)
- [ ] Настроен автостарт (`pm2 startup`)
- [ ] Мониторинг настроен

---

**Для развертывания обоих сервисов используйте:**
```bash
pm2 start /var/www/pda.repair/ecosystem.config.js
pm2 save
```
