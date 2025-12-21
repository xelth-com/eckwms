const API_URL = 'http://localhost:3100';
const DEVICE_ID = '4017627aae0996cc'; // Your device
const ADMIN_EMAIL = 'admin@eckwms.local';
const ADMIN_PASS = 'admin123';

// The "Magic" Layout - Inventory Mode
const INVENTORY_LAYOUT = {
    "components": [
        {
            "type": "text",
            "content": "📦 INVENTORY MODE",
            "style": "h1"
        },
        {
            "type": "card",
            "title": "Current Task",
            "content": "Aisle 4, Shelf B needs counting. Please scan all items on the shelf."
        },
        {
            "type": "spacing",
            "height": 20
        },
        {
            "type": "button",
            "label": "📷 Start Scanning Shelf",
            "action": "start_scan",
            "primary": true
        },
        {
            "type": "button",
            "label": "⚠️ Report Issue",
            "action": "report_issue",
            "primary": false
        }
    ]
};

async function runMagic() {
    console.log('✨ Preparing the magic show...');

    try {
        // 1. Login
        const loginRes = await fetch(`${API_URL}/auth/login`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ email: ADMIN_EMAIL, password: ADMIN_PASS })
        });
        const token = (await loginRes.json()).tokens.accessToken;

        // 2. Push Layout
        console.log('🚀 Sending UI Layout to device...');
        const pushRes = await fetch(`${API_URL}/admin/api/devices/${DEVICE_ID}/layout`, {
            method: 'POST',
            headers: {
                'Content-Type': 'application/json',
                'Authorization': `Bearer ${token}`
            },
            body: JSON.stringify({ layout: INVENTORY_LAYOUT })
        });

        const result = await pushRes.json();
        console.log('Response:', result);

        if (result.success) {
            console.log('\n🎉 TA-DA! Check your phone screen now!');
            console.log('The app should have switched to "AI Interface" automatically.');
        } else {
            console.log('❌ Failed. Is the device connected via WebSocket?');
        }

    } catch (error) {
        console.error('❌ Error:', error.message);
    }
}

runMagic();
