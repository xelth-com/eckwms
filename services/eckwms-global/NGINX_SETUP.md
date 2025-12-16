# eckWMS Global Server - Nginx Configuration

## Текущая конфигурация Nginx

Этот документ содержит **реальные** файлы конфигурации Nginx, используемые на продакшн сервере для `pda.repair` и микросервиса `eckwms-global`.

---

## 1. Основной конфиг Nginx

**Файл**: `/etc/nginx/nginx.conf`

```nginx
user www-data;
worker_processes auto;
pid /run/nginx.pid;
error_log /var/log/nginx/error.log;
include /etc/nginx/modules-enabled/*.conf;

events {
    worker_connections 768;
}

http {
    sendfile on;
    tcp_nopush on;
    types_hash_max_size 2048;

    include /etc/nginx/mime.types;
    default_type application/octet-stream;

    ssl_protocols TLSv1 TLSv1.2 TLSv1.3;
    ssl_prefer_server_ciphers on;

    access_log /var/log/nginx/access.log;

    gzip on;

    include /etc/nginx/conf.d/*.conf;
    include /etc/nginx/sites-enabled/*;
}
```

---

## 2. Конфигурация для pda.repair (с eckWMS)

**Файл**: `/etc/nginx/sites-available/pda.repair.conf`

```nginx
server {
    server_name pda.repair www.pda.repair;


    location /.well-known/acme-challenge/ { allow all; }

    # Certbot добавит сюда редирект на HTTPS

    listen [::]:443 ssl; # managed by Certbot
    listen 443 ssl; # managed by Certbot
    ssl_certificate /etc/letsencrypt/live/pda.repair/fullchain.pem; # managed by Certbot
    ssl_certificate_key /etc/letsencrypt/live/pda.repair/privkey.pem; # managed by Certbot
    include /etc/letsencrypt/options-ssl-nginx.conf; # managed by Certbot
    ssl_dhparam /etc/letsencrypt/ssl-dhparams.pem; # managed by Certbot


 # --- Логирование (оставляем как есть) ---
    access_log /var/log/nginx/pda.repair.access.log;
    error_log /var/log/nginx/pda.repair.error.log warn;

    # --- Заголовки безопасности (оставляем как есть) ---
    add_header X-Frame-Options "SAMEORIGIN" always;
    add_header X-Content-Type-Options "nosniff" always;
    add_header Referrer-Policy "strict-origin-when-cross-origin" always;

    # --- Обработка запросов (оставляем как есть) ---

# 1. Обрабатываем ТОЛЬКО статические ассеты (кроме HTML)
location ~* \.(?:css|js|png|jpg|jpeg|webp|avif|gif|ico|svg|woff|woff2|ttf|eot)$ {
    root /var/www/pda.repair/html;
    try_files $uri =404;
    expires max;
    add_header Cache-Control public;
}

# 2. Явно обрабатываем HTML файлы - отправляем их на Node.js
location ~* \.html$ {
    proxy_pass http://localhost:3000;
    proxy_http_version 1.1;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection 'upgrade';
    proxy_set_header Host $host;
    proxy_set_header X-Real-IP $remote_addr;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    proxy_set_header X-Forwarded-Proto $scheme;
    proxy_cache_bypass $http_upgrade;
}

# 2.5. Проксируем запросы к микросервису eckWMS Global Server
location /ECK/ {
    proxy_pass http://localhost:8080/ECK/;
    proxy_http_version 1.1;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection 'upgrade';
    proxy_set_header Host $host;
    proxy_set_header X-Real-IP $remote_addr;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    proxy_set_header X-Forwarded-Proto $scheme;
    proxy_cache_bypass $http_upgrade;
}

# 3. Все остальные запросы проксируем на Node.js
location / {
    proxy_pass http://localhost:3000;
    proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection 'upgrade';
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_cache_bypass $http_upgrade;
    }



}

server {
    if ($host = www.pda.repair) {
        return 301 https://$host$request_uri;
    } # managed by Certbot


    # listen 443 ssl http2; # Будет добавлено Certbot
    # listen [::]:443 ssl http2; # Будет добавлено Certbot
    server_name pda.repair www.pda.repair;

    # Корень для файлов этого сайта
    root /var/www/pda.repair/html;
    index index.html index.htm;

    # SSL настройки будут добавлены Certbot
    # ssl_certificate ...;
    # ssl_certificate_key ...;
    # include /etc/letsencrypt/options-ssl-nginx.conf;
    # ssl_dhparam ...;

    # Логи для этого сайта
    access_log /var/log/nginx/pda.repair.access.log;
    error_log /var/log/nginx/pda.repair.error.log warn;

    # Заголовки безопасности
    add_header X-Frame-Options "SAMEORIGIN" always;
    add_header X-Content-Type-Options "nosniff" always;
    add_header Referrer-Policy "strict-origin-when-cross-origin" always;

    # Обработка запросов к статическим файлам
    location / {
        try_files $uri $uri/ =404;
    }


}
server {
    if ($host = pda.repair) {
        return 301 https://$host$request_uri;
    } # managed by Certbot


    listen 80;
    listen [::]:80;
    server_name pda.repair www.pda.repair;
    return 404; # managed by Certbot


}
```

---

## Анализ конфигурации

### Структура серверов

1. **Главный HTTPS сервер** (строки 1-77)
   - Слушает порты 443 (SSL)
   - SSL сертификаты от Let's Encrypt (Certbot)
   - Обрабатывает все HTTPS запросы

2. **Редирект сервер** (строки 79-114)
   - Редиректит www.pda.repair → pda.repair

3. **HTTP сервер** (строки 115-128)
   - Редиректит HTTP → HTTPS

### Маршрутизация запросов

#### 1. Статические файлы (CSS, JS, изображения)
```nginx
location ~* \.(?:css|js|png|jpg|jpeg|webp|avif|gif|ico|svg|woff|woff2|ttf|eot)$ {
    root /var/www/pda.repair/html;
    try_files $uri =404;
    expires max;
    add_header Cache-Control public;
}
```
- Обслуживаются напрямую из `/var/www/pda.repair/html`
- Максимальное кеширование
- Без проксирования

#### 2. HTML файлы
```nginx
location ~* \.html$ {
    proxy_pass http://localhost:3000;
    # ... proxy headers ...
}
```
- Проксируются на Node.js сервер (порт 3000)
- Вероятно, для SSR или динамической генерации

#### 3. **eckWMS Global Server** (КРИТИЧНО)
```nginx
location /ECK/ {
    proxy_pass http://localhost:8080/ECK/;
    proxy_http_version 1.1;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection 'upgrade';
    proxy_set_header Host $host;
    proxy_set_header X-Real-IP $remote_addr;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    proxy_set_header X-Forwarded-Proto $scheme;
    proxy_cache_bypass $http_upgrade;
}
```

**Важные детали:**
- Все запросы к `/ECK/*` идут на микросервис `eckwms-global` (порт 8080)
- Заголовки `X-Real-IP` и `X-Forwarded-For` передают реальный IP клиента
- Поддержка WebSocket (Upgrade, Connection headers)
- `proxy_cache_bypass` отключает кеширование для динамических данных

#### 4. Все остальные запросы
```nginx
location / {
    proxy_pass http://localhost:3000;
    # ... proxy headers ...
}
```
- Проксируются на основной Node.js сервер pda.repair

---

## Важные proxy headers для eckWMS

Эти заголовки критичны для корректной работы определения IP:

| Header | Значение | Назначение |
|--------|----------|------------|
| `Host` | `$host` | Оригинальный домен (pda.repair) |
| `X-Real-IP` | `$remote_addr` | IP клиента (один адрес) |
| `X-Forwarded-For` | `$proxy_add_x_forwarded_for` | Цепочка прокси + клиент |
| `X-Forwarded-Proto` | `$scheme` | https/http |
| `Upgrade` | `$http_upgrade` | Для WebSocket |
| `Connection` | `'upgrade'` | Для WebSocket |

### Почему это работает с `app.set('trust proxy', 1)`

В `server.js` настроено:
```javascript
app.set('trust proxy', 1);
```

Это заставляет Express:
1. Читать `X-Forwarded-For` заголовок
2. Использовать первый IP из цепочки как `req.ip`
3. Игнорировать IP Nginx (127.0.0.1)

---

## Endpoints eckWMS Global Server

Все endpoints доступны через `https://pda.repair/ECK/...`:

- `GET /ECK/HEALTH` - Health check
- `GET /ECK/PROXY/HEALTH` - Proxy health (без требования заголовков)
- `POST /ECK/API/INTERNAL/REGISTER-INSTANCE` - Регистрация инстанса
- `POST /ECK/API/INTERNAL/GET-INSTANCE-INFO` - Получение информации об инстансе
- `POST /ECK/API/DEVICE/REGISTER` - Регистрация устройства
- `POST /ECK/PROXY` - Прокси эндпоинт (требует `X-eckWMS-Target-Url`)
- `GET /ECK/:code` - QR коды

---

## Безопасность

### Текущие настройки безопасности

```nginx
add_header X-Frame-Options "SAMEORIGIN" always;
add_header X-Content-Type-Options "nosniff" always;
add_header Referrer-Policy "strict-origin-when-cross-origin" always;
```

- **X-Frame-Options**: Защита от clickjacking
- **X-Content-Type-Options**: Запрет MIME sniffing
- **Referrer-Policy**: Контроль передачи referrer

### SSL/TLS

- **Сертификаты**: Let's Encrypt (автообновление через Certbot)
- **Протоколы**: TLSv1, TLSv1.2, TLSv1.3
- **Конфиг**: `/etc/letsencrypt/options-ssl-nginx.conf`

---

## Логирование

### Логи Nginx

```nginx
access_log /var/log/nginx/pda.repair.access.log;
error_log /var/log/nginx/pda.repair.error.log warn;
```

### Логи приложения

- **eckWMS Global**: `pm2 logs eckwms-global`
- **Node.js (pda.repair)**: `pm2 logs pda.repair`

---

## Проверка конфигурации

```bash
# Проверка синтаксиса
nginx -t

# Перезагрузка без даунтайма
nginx -s reload
# или
systemctl reload nginx

# Проверка активных портов
netstat -tulpn | grep nginx
netstat -tulpn | grep 8080
netstat -tulpn | grep 3000

# Тест endpoints
curl -I https://pda.repair/ECK/HEALTH
curl -I https://pda.repair/ECK/PROXY/HEALTH
```

---

## Управление сервисами

### PM2 процессы

```bash
# Список процессов
pm2 list

# Логи eckWMS
pm2 logs eckwms-global

# Рестарт
pm2 restart eckwms-global

# Статус
pm2 show eckwms-global
```

### Nginx

```bash
# Статус
systemctl status nginx

# Рестарт
systemctl restart nginx

# Reload конфигурации
systemctl reload nginx
```

---

## Файловая структура

```
/etc/nginx/
├── nginx.conf                      # Главный конфиг
├── sites-available/
│   └── pda.repair.conf            # Конфиг pda.repair + eckWMS
├── sites-enabled/
│   └── pda.repair.conf -> ../sites-available/pda.repair.conf

/var/www/
├── pda.repair/
│   └── html/                       # Статические файлы сайта
└── eckwms/
    └── services/
        └── eckwms-global/
            ├── src/server.js       # Express приложение (порт 8080)
            ├── .env                # Переменные окружения
            └── NGINX_SETUP.md      # Этот файл

/var/log/nginx/
├── pda.repair.access.log          # Access логи
├── pda.repair.error.log           # Error логи
└── error.log                       # Общие ошибки Nginx

/etc/letsencrypt/
└── live/
    └── pda.repair/
        ├── fullchain.pem           # SSL сертификат
        └── privkey.pem             # Приватный ключ
```

---

## Диагностика проблем

### Проблема: 502 Bad Gateway на /ECK/*

**Проверить:**
```bash
# Работает ли приложение?
pm2 list | grep eckwms-global

# Слушает ли порт 8080?
netstat -tulpn | grep 8080

# Есть ли ошибки в логах?
pm2 logs eckwms-global --lines 50
tail -50 /var/log/nginx/pda.repair.error.log
```

### Проблема: Неправильный IP в логах

**Проверить:**
```bash
# Включен ли trust proxy?
grep "trust proxy" /var/www/eckwms/services/eckwms-global/src/server.js

# Передает ли Nginx заголовки?
curl -v https://pda.repair/ECK/HEALTH 2>&1 | grep -i forward

# Что видит приложение?
pm2 logs eckwms-global | grep "IP Detection"
```

---

## История изменений

- **2025-12-16**: Добавлена настройка `trust proxy` в server.js
- **2025-12-16**: Обновлены все API пути на uppercase (HEALTH, PROXY, API)
- **2025-12-16**: Добавлен bypass для /PROXY/HEALTH без заголовков
- **2025-04-04**: Базовая конфигурация Nginx для pda.repair
- **Ранее**: Интеграция eckWMS Global Server на порт 8080

---

**Архитектор**: Используй эту информацию для понимания текущей инфраструктуры и планирования изменений.
