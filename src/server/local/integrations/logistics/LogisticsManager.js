const OpalDriver = require('./drivers/opal/OpalDriver');

class LogisticsManager {
    constructor() {
        this.drivers = new Map();
        this._initializeDrivers();
    }

    _initializeDrivers() {
        // Register OPAL driver if config exists
        if (process.env.OPAL_USERNAME && process.env.OPAL_PASSWORD) {
            try {
                const opalDriver = new OpalDriver({
                    username: process.env.OPAL_USERNAME,
                    password: process.env.OPAL_PASSWORD,
                    url: process.env.OPAL_URL || 'https://opal-kurier.de',
                    cookiesPath: process.env.OPAL_COOKIES_PATH || '../data/opal-cookies.json'
                });
                this.registerDriver(opalDriver);
            } catch (error) {
                console.error('[Logistics] Failed to initialize OPAL driver:', error.message);
            }
        }
    }

    registerDriver(driver) {
        this.drivers.set(driver.name, driver);
        console.log(`[Logistics] Registered driver: ${driver.name}`);
    }

    getDriver(name) {
        const driver = this.drivers.get(name);
        if (!driver) {
            throw new Error(`Driver '${name}' not found. Available drivers: ${Array.from(this.drivers.keys()).join(', ')}`);
        }
        return driver;
    }

    /**
     * Auto-select driver based on rules (e.g. weight, destination)
     */
    suggestDriver(shipmentData) {
        // Simple rule: default to 'opal' if available
        if (this.drivers.has('opal')) {
            return this.getDriver('opal');
        }

        // Return first available driver
        const firstDriver = Array.from(this.drivers.values())[0];
        if (!firstDriver) {
            throw new Error('No logistics drivers available');
        }

        return firstDriver;
    }
}

module.exports = new LogisticsManager();
