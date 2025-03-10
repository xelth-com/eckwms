// create-admin.js
const fs = require('fs');
const path = require('path');
const crypto = require('crypto');

// Обеспечение существования директории
const baseDir = path.join(__dirname, 'base');
if (!fs.existsSync(baseDir)) {
    fs.mkdirSync(baseDir, { recursive: true });
}

// Создание администратора
const createAdmin = () => {
    // Генерация хеша пароля
    const password = 'admin123';
    const salt = crypto.randomBytes(16).toString('hex');
    const hash = crypto.pbkdf2Sync(password, salt, 1000, 64, 'sha512').toString('hex');
    const hashedPassword = `${salt}:${hash}`;
    
    // Создание объекта пользователя
    const timestamp = Math.floor(Date.now() / 1000);
    const user = {
        sn: ["u000000000000000001", timestamp],
        nm: "admin",
        pwd: hashedPassword,
        cem: "admin@example.com",
        r: "a",  // Роль администратора
        active: true
    };
    
    // Запись файла пользователей
    const usersFile = path.join(baseDir, 'users.json');
    fs.writeFileSync(usersFile, JSON.stringify(user) + '\n');
    
    console.log(`Администратор успешно создан в ${usersFile}`);
    console.log(`Логин: admin`);
    console.log(`Пароль: admin123`);
};

createAdmin();