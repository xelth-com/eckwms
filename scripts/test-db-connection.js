// scripts/test-db-connection.js
require('dotenv').config();
const { Client } = require('pg');

const client = new Client({
  user: process.env.PG_USERNAME,
  host: process.env.PG_HOST,
  database: process.env.PG_DATABASE, 
  password: process.env.PG_PASSWORD,
  port: process.env.PG_PORT,
});

async function testConnection() {
  try {
    await client.connect();
    console.log('Успешное подключение к базе данных');
    
    // Запрос для получения списка таблиц
    const tablesResult = await client.query(`
      SELECT tablename AS table_name 
      FROM pg_catalog.pg_tables 
      WHERE schemaname = 'public'
    `);
    
    console.log('Список таблиц:');
    
    // Вывод всей структуры первой строки для отладки
    if (tablesResult.rows.length > 0) {
      console.log('Структура первой строки:', JSON.stringify(tablesResult.rows[0]));
    }
    
    // Перебираем строки результата
    tablesResult.rows.forEach(row => {
      // Используем правильное имя поля из структуры
      console.log(`- ${row.table_name}`);
    });

    // Проверим структуру таблиц
    console.log('\nСтруктура таблицы user_auth:');
    const userAuthColumns = await client.query(`
      SELECT column_name, data_type 
      FROM information_schema.columns
      WHERE table_name = 'user_auth'
    `);
    
    userAuthColumns.rows.forEach(column => {
      console.log(`- ${column.column_name}: ${column.data_type}`);
    });
    
  } catch (error) {
    console.error('Ошибка подключения:', error);
  } finally {
    await client.end();
  }
}

testConnection();