#!/usr/bin/env node

/**
 * Script to create a test admin user
 * Usage: node scripts/create-admin.js
 */

require('dotenv').config();
const db = require('../src/shared/models/postgresql');

async function createAdmin() {
  try {
    console.log('🔍 Checking database connection...');
    await db.sequelize.authenticate();
    console.log('✅ Database connected\n');

    // Check if admin already exists
    const existingAdmin = await db.UserAuth.findOne({
      where: { email: 'admin@eckwms.local' }
    });

    if (existingAdmin) {
      console.log('ℹ️  Admin user already exists!');
      console.log('📧 Email: admin@eckwms.local');
      console.log('🔑 Password: admin123\n');

      // Update role to admin if needed
      if (existingAdmin.role !== 'admin') {
        existingAdmin.role = 'admin';
        await existingAdmin.save();
        console.log('✅ Updated user role to admin\n');
      }

      process.exit(0);
    }

    console.log('👤 Creating admin user...');

    const admin = await db.UserAuth.create({
      username: 'admin',
      email: 'admin@eckwms.local',
      password: 'admin123', // Will be hashed by model hooks
      name: 'Administrator',
      company: 'eckWMS',
      role: 'admin',
      userType: 'company',
      isActive: true
    });

    console.log('✅ Admin user created successfully!\n');
    console.log('📋 Login Credentials:');
    console.log('━━━━━━━━━━━━━━━━━━━━━━━━━━━━');
    console.log('📧 Email:    admin@eckwms.local');
    console.log('🔑 Password: admin123');
    console.log('━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n');
    console.log('🌐 Login URL: http://localhost:3100/auth/login\n');

    await db.sequelize.close();
    process.exit(0);
  } catch (error) {
    console.error('❌ Error:', error.message);
    process.exit(1);
  }
}

createAdmin();
