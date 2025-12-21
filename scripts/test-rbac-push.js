const fetch = require('node-fetch'); // Assuming node-fetch v2 or native fetch in Node 18+

const API_URL = 'http://localhost:3100';
const DEVICE_ID = '4017627aae0996cc'; // Your specific device ID
const ADMIN_EMAIL = 'admin@eckwms.local';
const ADMIN_PASS = 'admin123';

async function runTest() {
    console.log('🧪 Starting RBAC Push Integration Test...');

    try {
        // 1. Login as Admin
        console.log('🔑 Logging in as Admin...');
        const loginRes = await fetch(`${API_URL}/auth/login`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ email: ADMIN_EMAIL, password: ADMIN_PASS })
        });

        if (!loginRes.ok) throw new Error(`Login failed: ${loginRes.statusText}`);
        const loginData = await loginRes.json();
        const token = loginData.tokens.accessToken;
        console.log('✅ Logged in.');

        // 2. Get Roles to find 'MANAGER' ID
        console.log('📋 Fetching Roles...');
        const rolesRes = await fetch(`${API_URL}/api/rbac/roles`, {
            headers: { 'Authorization': `Bearer ${token}` }
        });
        const roles = await rolesRes.json();
        const managerRole = roles.find(r => r.name === 'MANAGER');

        if (!managerRole) throw new Error('MANAGER role not found in DB. Run migration 004.');
        console.log(`✅ Found MANAGER Role ID: ${managerRole.id}`);

        // 3. Assign Role to Device
        console.log(`🚀 Assigning MANAGER role to device ${DEVICE_ID}...`);
        const assignRes = await fetch(`${API_URL}/admin/api/devices/${DEVICE_ID}/role`, {
            method: 'POST',
            headers: {
                'Content-Type': 'application/json',
                'Authorization': `Bearer ${token}`
            },
            body: JSON.stringify({ roleId: managerRole.id })
        });

        const result = await assignRes.json();
        if (!assignRes.ok) throw new Error(`Assignment failed: ${JSON.stringify(result)}`);

        console.log('✅ Role assigned successfully!');
        console.log('📤 Server should have triggered WebSocket Push.');
        console.log('👀 Check Android logs for: "⚡ Received PUSH ROLE_UPDATE"');
        console.log('ℹ️ Permissions pushed:', result.permissions);

    } catch (error) {
        console.error('❌ TEST FAILED:', error.message);
    }
}

runTest();
