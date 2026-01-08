#!/bin/bash

# Test script for Device Pairing System
# This script helps verify that all components are working correctly

echo "========================================="
echo "Device Pairing System - Test Script"
echo "========================================="
echo ""

# Colors for output
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Function to print status
print_status() {
    if [ $1 -eq 0 ]; then
        echo -e "${GREEN}✓${NC} $2"
    else
        echo -e "${RED}✗${NC} $2"
    fi
}

# 1. Check if PostgreSQL is accessible
echo "1. Проверка подключения к PostgreSQL..."
nc -zv 172.29.16.1 5432 &>/dev/null
print_status $? "PostgreSQL доступен на 172.29.16.1:5432"
echo ""

# 2. Check if .env file exists and has required keys
echo "2. Проверка конфигурации .env..."
if [ -f .env ]; then
    print_status 0 ".env файл найден"

    # Check for required environment variables
    if grep -q "SERVER_PUBLIC_KEY=" .env && grep -q "SERVER_PRIVATE_KEY=" .env; then
        print_status 0 "Ключи шифрования настроены"
    else
        print_status 1 "Ключи шифрования НЕ настроены"
    fi

    if grep -q "JWT_SECRET=" .env; then
        print_status 0 "JWT_SECRET настроен"
    else
        print_status 1 "JWT_SECRET НЕ настроен"
    fi
else
    print_status 1 ".env файл НЕ найден"
fi
echo ""

# 3. Check if required files exist
echo "3. Проверка файлов системы..."
files=(
    "src/server/local/views/admin/pairing.html"
    "public/js/admin_pairing.js"
    "src/server/local/routes/setup.js"
)

for file in "${files[@]}"; do
    if [ -f "$file" ]; then
        print_status 0 "$file"
    else
        print_status 1 "$file (файл отсутствует)"
    fi
done
echo ""

# 4. Check if servers are running
echo "4. Проверка запущенных серверов..."
LOCAL_RUNNING=$(lsof -ti:3100 2>/dev/null)
GLOBAL_RUNNING=$(lsof -ti:8080 2>/dev/null)

if [ -n "$LOCAL_RUNNING" ]; then
    print_status 0 "Локальный сервер работает (порт 3100, PID: $LOCAL_RUNNING)"
else
    print_status 1 "Локальный сервер НЕ работает (порт 3100)"
fi

if [ -n "$GLOBAL_RUNNING" ]; then
    print_status 0 "Глобальный сервер работает (порт 8080, PID: $GLOBAL_RUNNING)"
else
    print_status 1 "Глобальный сервер НЕ работает (порт 8080)"
fi
echo ""

# 5. If servers are running, test endpoints
if [ -n "$LOCAL_RUNNING" ] && [ -n "$GLOBAL_RUNNING" ]; then
    echo "5. Проверка эндпоинтов..."

    # Test global server health
    HEALTH_CHECK=$(curl -s -o /dev/null -w "%{http_code}" http://localhost:8080/health)
    if [ "$HEALTH_CHECK" = "200" ]; then
        print_status 0 "Глобальный сервер /health отвечает"
    else
        print_status 1 "Глобальный сервер /health НЕ отвечает (код: $HEALTH_CHECK)"
    fi

    echo ""
fi

echo "========================================="
echo "Тестирование завершено!"
echo "========================================="
echo ""

# Print instructions
if [ -z "$LOCAL_RUNNING" ] || [ -z "$GLOBAL_RUNNING" ]; then
    echo -e "${YELLOW}Для запуска серверов используйте:${NC}"
    echo ""
    if [ -z "$LOCAL_RUNNING" ]; then
        echo "  Терминал 1: npm run start:local"
    fi
    if [ -z "$GLOBAL_RUNNING" ]; then
        echo "  Терминал 2: npm run start:global"
    fi
    echo ""
fi

echo -e "${YELLOW}Следующие шаги для тестирования:${NC}"
echo ""
echo "1. Запустите оба сервера (если еще не запущены)"
echo "2. Войдите в систему как администратор: http://localhost:3100/auth/login"
echo "3. Перейдите на страницу подключения: http://localhost:3100/admin/pairing"
echo "4. Нажмите 'Generate New QR Code'"
echo "5. Проверьте, что QR код появился и статус показывает 'ONLINE'"
echo ""
