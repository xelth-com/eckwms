// Test database connection for eckWMS integration with InBody
require('dotenv').config();
const db = require('./src/shared/models/postgresql');

async function testConnection() {
  console.log('🔍 Testing eckWMS database connection...\n');

  try {
    // Test 1: Basic connection
    console.log('1️⃣ Testing database connection...');
    await db.sequelize.authenticate();
    console.log('✅ Database connection established successfully!');
    console.log(`   Database: ${process.env.PG_DATABASE}`);
    console.log(`   User: ${process.env.PG_USERNAME}`);
    console.log(`   Host: ${process.env.PG_HOST}:${process.env.PG_PORT}\n`);

    // Test 2: Count scans
    console.log('2️⃣ Testing Scan model...');
    const scanCount = await db.Scan.count();
    console.log(`✅ Scans table accessible: ${scanCount} records\n`);

    // Test 3: Get recent scans
    console.log('3️⃣ Fetching recent scans...');
    const recentScans = await db.Scan.findAll({
      limit: 3,
      order: [['createdAt', 'DESC']],
      attributes: ['id', 'deviceId', 'payload', 'status', 'createdAt']
    });
    console.log(`✅ Found ${recentScans.length} recent scans:`);
    recentScans.forEach((scan, i) => {
      console.log(`   ${i+1}. Scan ID: ${scan.id}`);
      console.log(`      Device: ${scan.deviceId || 'N/A'}`);
      console.log(`      Payload: ${scan.payload}`);
      console.log(`      Status: ${scan.status}`);
      console.log(`      Created: ${scan.createdAt}`);
    });
    console.log();

    // Test 4: Count eckwms_instances
    console.log('4️⃣ Testing EckwmsInstance model...');
    const instanceCount = await db.EckwmsInstance.count();
    console.log(`✅ EckwmsInstances table accessible: ${instanceCount} records\n`);

    // Test 5: Get instances
    console.log('5️⃣ Fetching eckWMS instances...');
    const instances = await db.EckwmsInstance.findAll({
      attributes: ['id', 'name', 'server_url', 'tier']
    });
    console.log(`✅ Found ${instances.length} instances:`);
    instances.forEach((inst, i) => {
      console.log(`   ${i+1}. ${inst.name}`);
      console.log(`      URL: ${inst.server_url}`);
      console.log(`      Tier: ${inst.tier}`);
    });
    console.log();

    // Test 6: Count repair orders
    console.log('6️⃣ Testing RepairOrder model (InBody Driver)...');
    const repairOrderCount = await db.RepairOrder.count();
    console.log(`✅ RepairOrders table accessible: ${repairOrderCount} records\n`);

    // Test 7: Get repair orders with scans
    console.log('7️⃣ Testing Scan ↔ RepairOrder relationship...');
    const repairOrdersWithScans = await db.RepairOrder.findAll({
      where: { scan_id: { [db.Sequelize.Op.ne]: null } },
      include: [{
        model: db.Scan,
        as: 'scan',
        attributes: ['id', 'deviceId', 'payload', 'status']
      }],
      limit: 3
    });
    console.log(`✅ Found ${repairOrdersWithScans.length} repair orders linked to scans`);
    if (repairOrdersWithScans.length === 0) {
      console.log('   ℹ️  No repair orders are currently linked to scans');
    }
    console.log();

    // Test 8: Raw query test (check view)
    console.log('8️⃣ Testing integrated view...');
    const [results] = await db.sequelize.query(
      'SELECT COUNT(*) as total FROM v_scans_with_repairs'
    );
    console.log(`✅ View v_scans_with_repairs accessible: ${results[0].total} records\n`);

    // Summary
    console.log('='.repeat(60));
    console.log('✅ ALL TESTS PASSED!');
    console.log('='.repeat(60));
    console.log('📊 Summary:');
    console.log(`   • Database: ${process.env.PG_DATABASE}`);
    console.log(`   • Scans: ${scanCount}`);
    console.log(`   • Instances: ${instanceCount}`);
    console.log(`   • Repair Orders: ${repairOrderCount}`);
    console.log(`   • Linked Orders: ${repairOrdersWithScans.length}`);
    console.log();
    console.log('🎉 eckWMS is successfully integrated with InBody database!');
    console.log();

  } catch (error) {
    console.error('❌ Error during testing:');
    console.error(error.message);
    if (error.parent) {
      console.error('Database error:', error.parent.message);
    }
    process.exit(1);
  } finally {
    await db.sequelize.close();
  }
}

// Run the test
testConnection();
