require('dotenv').config();
const FirewallManager = require('../src/server/local/utils/firewallManager');

async function run() {
    const port = process.env.LOCAL_SERVER_PORT || process.env.PORT || 3100;
    console.log(`🔍 Checking network accessibility for port ${port}...`);

    const isOpen = await FirewallManager.isPortOpen(port);

    if (isOpen) {
        console.log('✅ Port is accessible.');
    } else {
        console.log('⚠️ Port appears closed or UFW status check failed.');
        console.log('Attempting to open port automatically...');
        await FirewallManager.openPort(port);

        // Re-check
        const isNowOpen = await FirewallManager.isPortOpen(port);
        if (isNowOpen) {
            console.log('✅ Port successfully opened!');
        } else {
            console.log('❌ Failed to verify port opening. You may need to run:');
            console.log(`   sudo ufw allow ${port}/tcp`);
        }
    }
}

run();
