#!/usr/bin/env node

/**
 * Script to list all users and update admin role
 */

require('dotenv').config();
const db = require('../src/shared/models/postgresql');

async function listUsers() {
  try {
    console.log('🔍 Connecting to database...');
    await db.sequelize.authenticate();
    console.log('✅ Connected\n');

    const users = await db.UserAuth.findAll({
      attributes: ['id', 'username', 'email', 'role', 'isActive']
    });

    if (users.length === 0) {
      console.log('ℹ️  No users found in database\n');
    } else {
      console.log('📋 Users in database:');
      console.log('━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━');
      users.forEach(user => {
        console.log(`ID: ${user.id}`);
        console.log(`👤 Username: ${user.username}`);
        console.log(`📧 Email: ${user.email}`);
        console.log(`🔑 Role: ${user.role}`);
        console.log(`✓ Active: ${user.isActive}`);
        console.log('─────────────────────────────────────────────────────────');
      });
      console.log('');
    }

    // Check if there's an admin
    const adminUser = users.find(u => u.role === 'admin');

    if (!adminUser) {
      console.log('⚠️  No admin user found. Promoting first user to admin...\n');

      if (users.length > 0) {
        const firstUser = await db.UserAuth.findByPk(users[0].id);
        firstUser.role = 'admin';
        await firstUser.save();

        console.log('✅ User promoted to admin:');
        console.log(`📧 Email: ${firstUser.email}`);
        console.log(`👤 Username: ${firstUser.username}\n`);
      } else {
        console.log('❌ No users to promote. Please register first.\n');
      }
    } else {
      console.log('✅ Admin user found:');
      console.log(`📧 Email: ${adminUser.email}`);
      console.log(`👤 Username: ${adminUser.username}\n`);
    }

    await db.sequelize.close();
    process.exit(0);
  } catch (error) {
    console.error('❌ Error:', error.message);
    process.exit(1);
  }
}

listUsers();
