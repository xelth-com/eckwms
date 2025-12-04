#!/usr/bin/env node

/**
 * Script to reset admin password
 */

require('dotenv').config();
const db = require('../src/shared/models/postgresql');

async function resetPassword() {
  try {
    console.log('🔍 Connecting to database...\n');
    await db.sequelize.authenticate();

    const admin = await db.UserAuth.findOne({
      where: { email: 'admin@example.com' }
    });

    if (!admin) {
      console.log('❌ Admin user not found\n');
      process.exit(1);
    }

    // Update password (will be hashed by model hooks)
    admin.password = 'admin123';
    await admin.save();

    console.log('✅ Admin password reset successfully!\n');
    console.log('━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━');
    console.log('📋 Login Credentials:');
    console.log('━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━');
    console.log('📧 Email:    admin@example.com');
    console.log('🔑 Password: admin123');
    console.log('━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n');
    console.log('🌐 Login URL: http://localhost:3100/auth/login\n');
    console.log('📝 Steps to login:');
    console.log('   1. Open http://localhost:3100/auth/login in browser');
    console.log('   2. Enter email: admin@example.com');
    console.log('   3. Enter password: admin123');
    console.log('   4. Click Login\n');

    await db.sequelize.close();
    process.exit(0);
  } catch (error) {
    console.error('❌ Error:', error.message);
    process.exit(1);
  }
}

resetPassword();
