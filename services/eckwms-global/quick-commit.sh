#!/bin/bash
# Quick commit for microservice only
# Usage: ./quick-commit.sh "commit message"

if [ -z "$1" ]; then
    echo "Usage: ./quick-commit.sh \"commit message\""
    exit 1
fi

cd /var/www/eckwms
git add services/eckwms-global/
git commit -m "$1"
git push

echo "✓ Committed and pushed microservice changes"
