#!/bin/bash
# Helper script to commit only eckwms-global microservice changes

cd /var/www/eckwms

# Show only microservice changes
echo "=== Изменения в микросервисе ==="
git status services/eckwms-global/

echo ""
read -p "Добавить эти файлы в коммит? (y/n) " -n 1 -r
echo

if [[ $REPLY =~ ^[Yy]$ ]]; then
    # Add only microservice files
    git add services/eckwms-global/

    echo "Файлы добавлены. Введите сообщение коммита:"
    read -r commit_message

    if [ -n "$commit_message" ]; then
        git commit -m "$commit_message"

        read -p "Push в origin? (y/n) " -n 1 -r
        echo
        if [[ $REPLY =~ ^[Yy]$ ]]; then
            git push
        fi
    else
        echo "Сообщение коммита пустое, отменяем"
        git reset
    fi
fi
