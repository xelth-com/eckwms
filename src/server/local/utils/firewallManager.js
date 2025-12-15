const { exec } = require('child_process');
const util = require('util');
const execAsync = util.promisify(exec);
const os = require('os');

class FirewallManager {
    /**
     * Check if a specific port is allowed in UFW
     * @param {number} port
     * @returns {Promise<boolean>}
     */
    static async isPortOpen(port) {
        if (os.platform() !== 'linux') {
            console.log('[Firewall] Non-linux platform, skipping UFW check.');
            return true;
        }

        try {
            // Check if UFW is installed and active
            const { stdout } = await execAsync('sudo ufw status');

            if (stdout.includes('Status: inactive')) {
                console.log('[Firewall] UFW is inactive. All ports are open.');
                return true;
            }

            // Check for specific rule (e.g., '3100/tcp' ... 'ALLOW')
            // We look for the port followed by /tcp and verify 'ALLOW' is in the line
            const lines = stdout.split('\n');
            const portRule = lines.find(line => line.includes(`${port}/tcp`) && line.includes('ALLOW'));

            return !!portRule;
        } catch (error) {
            console.error('[Firewall] Error checking status (sudo might be required):', error.message);
            // Fail safe: assume false to trigger an attempt or manual check
            return false;
        }
    }

    /**
     * Attempt to open a port using UFW
     * @param {number} port
     */
    static async openPort(port) {
        if (os.platform() !== 'linux') return;

        console.log(`[Firewall] Attempting to open port ${port}...`);
        try {
            const { stdout } = await execAsync(`sudo ufw allow ${port}/tcp`);
            console.log(`[Firewall] Success: ${stdout.trim()}`);
            return true;
        } catch (error) {
            console.error(`[Firewall] Failed to open port (ensure you have sudo privileges): ${error.message}`);
            return false;
        }
    }
}

module.exports = FirewallManager;
