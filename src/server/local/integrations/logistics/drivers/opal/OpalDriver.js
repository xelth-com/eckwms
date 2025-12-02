const BaseLogisticsDriver = require('../../BaseLogisticsDriver');
const { chromium } = require('playwright');

/**
 * OPAL Kurier Driver
 *
 * Integrates with OPAL Kurier web system using Playwright automation.
 * Adapted from service-center-server/scripts/create-opal-order.js
 */
class OpalDriver extends BaseLogisticsDriver {
    constructor(config) {
        super(config);
        this.timeout = config.timeout || 300000; // 5 minutes default
        this.verbose = config.verbose || false;
        this.headless = config.headless !== false; // headless by default
    }

    get name() {
        return 'opal';
    }

    /**
     * Logger utility
     */
    log(message, level = 'info') {
        const timestamp = new Date().toISOString();
        const prefix = {
            'info': '✓',
            'warn': '⚠',
            'error': '✗',
            'debug': '•'
        }[level] || '•';

        if (level === 'debug' && !this.verbose) return;

        console.log(`[${timestamp}] [OPAL] ${prefix} ${message}`);
    }

    /**
     * Maps standardized eckWMS shipment data to internal format if needed
     * Supports both direct OPAL format and eckWMS warehouse format
     */
    _normalizeShipmentData(data) {
        // If data already has sender/recipient/package structure, return as-is
        if (data.sender && data.recipient && data.package) {
            return data;
        }

        // Otherwise, map from eckWMS format
        return {
            sender: {
                name: data.sender?.company || data.sender?.name || data.pickupName1,
                name2: data.sender?.name2 || data.pickupName2,
                contact: data.sender?.contact || data.pickupContact,
                street: data.sender?.street || data.pickupStreet,
                houseNumber: data.sender?.houseNumber || data.pickupHouseNumber,
                zip: data.sender?.zip || data.pickupZip,
                city: data.sender?.city || data.pickupCity,
                country: data.sender?.country || data.pickupCountry,
                phoneCountry: data.sender?.phoneCountry || data.pickupPhoneCountry,
                phoneArea: data.sender?.phoneArea || data.pickupPhoneArea,
                phoneNumber: data.sender?.phoneNumber || data.pickupPhoneNumber || data.sender?.phone,
                email: data.sender?.email || data.pickupEmail,
                notes: data.sender?.notes || data.pickupHinweis
            },
            recipient: {
                name: data.recipient?.company || data.recipient?.name || data.deliveryName1,
                name2: data.recipient?.name2 || data.deliveryName2,
                contact: data.recipient?.contact || data.deliveryContact,
                street: data.recipient?.street || data.deliveryStreet,
                houseNumber: data.recipient?.houseNumber || data.deliveryHouseNumber,
                zip: data.recipient?.zip || data.deliveryZip,
                city: data.recipient?.city || data.deliveryCity,
                country: data.recipient?.country || data.deliveryCountry,
                phoneCountry: data.recipient?.phoneCountry || data.deliveryPhoneCountry,
                phoneArea: data.recipient?.phoneArea || data.deliveryPhoneArea,
                phoneNumber: data.recipient?.phoneNumber || data.deliveryPhoneNumber || data.recipient?.phone,
                email: data.recipient?.email || data.deliveryEmail,
                notes: data.recipient?.notes || data.deliveryHinweis
            },
            package: {
                count: data.package?.count || data.packageCount || (data.packages ? data.packages.length : 1),
                weight: data.package?.weight || data.packageWeight || data.totalWeight ||
                        (data.packages ? data.packages.reduce((acc, p) => acc + (p.weight || 0), 0) : 0),
                description: data.package?.description || data.packageDescription,
                value: data.package?.value || data.shipmentValue || data.declaredValue || 2500,
                valueCurrency: data.package?.valueCurrency || data.shipmentValueCurrency || 'EUR'
            },
            options: {
                orderType: data.options?.orderType || data.orderType,
                vehicleType: data.options?.vehicleType || data.vehicleType,
                pickupDate: data.options?.pickupDate || data.pickupDate,
                pickupTimeFrom: data.options?.pickupTimeFrom || data.pickupTimeFrom,
                pickupTimeTo: data.options?.pickupTimeTo || data.pickupTimeTo,
                deliveryDate: data.options?.deliveryDate || data.deliveryDate,
                deliveryTimeFrom: data.options?.deliveryTimeFrom || data.deliveryTimeFrom,
                deliveryTimeTo: data.options?.deliveryTimeTo || data.deliveryTimeTo,
                refNumber: data.options?.refNumber || data.refNumber || data.orderId,
                notes: data.options?.notes || data.notes
            }
        };
    }

    /**
     * Initialize browser
     */
    async initializeBrowser() {
        this.log('Launching browser...', 'debug');

        const browser = await chromium.launch({
            headless: this.headless,
            args: ['--no-sandbox', '--disable-setuid-sandbox']
        });

        const context = await browser.newContext({
            viewport: { width: 1920, height: 1080 },
            userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36'
        });

        const page = await context.newPage();

        this.log('Browser initialized', 'info');

        return { browser, context, page };
    }

    /**
     * Perform login if needed
     */
    async performLogin(page) {
        this.log('Login required - performing automatic login...', 'info');

        if (!this.config.username || !this.config.password) {
            throw new Error('OPAL username and password must be configured for automatic login');
        }

        // Find and fill username field
        const usernameSelectors = [
            'input[name="username"]',
            'input[name="email"]',
            'input[type="email"]',
            'input[id*="user"]',
            'input[id*="email"]'
        ];

        let usernameFilled = false;
        for (const selector of usernameSelectors) {
            try {
                const field = await page.locator(selector).first();
                if (await field.count() > 0 && await field.isVisible()) {
                    await field.fill(this.config.username);
                    this.log('Username filled', 'debug');
                    usernameFilled = true;
                    break;
                }
            } catch (e) {
                // Try next selector
            }
        }

        if (!usernameFilled) {
            // Try first visible text input
            const textInputs = await page.locator('input[type="text"], input[type="email"], input:not([type])').all();
            for (const input of textInputs) {
                if (await input.isVisible()) {
                    await input.fill(this.config.username);
                    this.log('Username filled in first visible input', 'debug');
                    usernameFilled = true;
                    break;
                }
            }
        }

        if (!usernameFilled) {
            throw new Error('Could not find username field for login');
        }

        // Fill password
        const passwordField = await page.locator('input[type="password"]').first();
        if (await passwordField.count() === 0) {
            throw new Error('Could not find password field for login');
        }
        await passwordField.fill(this.config.password);
        this.log('Password filled', 'debug');

        // Submit form
        const buttonSelectors = [
            'button[type="submit"]',
            'input[type="submit"]',
            'button:has-text("Login")',
            'button:has-text("Anmelden")'
        ];

        let buttonClicked = false;
        for (const selector of buttonSelectors) {
            try {
                const button = await page.locator(selector).first();
                if (await button.count() > 0 && await button.isVisible()) {
                    await button.click();
                    this.log('Login submitted', 'debug');
                    buttonClicked = true;
                    break;
                }
            } catch (e) {
                // Try next selector
            }
        }

        if (!buttonClicked) {
            await passwordField.press('Enter');
            this.log('Login submitted via Enter key', 'debug');
        }

        // Wait for navigation
        await page.waitForLoadState('networkidle', { timeout: 30000 });
        await page.waitForTimeout(2000);

        this.log('Login completed', 'info');
    }

    /**
     * Navigate to OPAL and wait for frameset
     */
    async navigateToOpal(page) {
        this.log('Navigating to OPAL...', 'debug');

        await page.goto(this.config.url, { waitUntil: 'networkidle', timeout: this.timeout });

        // Wait for frameset to load
        try {
            await page.waitForSelector('frameset, frame[name="optop"]', { timeout: 30000 });
            this.log('OPAL frameset loaded', 'info');
        } catch (e) {
            // Check for login form
            const hasLoginForm = await page.locator('input[type="password"]').count();
            if (hasLoginForm > 0) {
                this.log('Login form detected - attempting automatic login...', 'info');
                await this.performLogin(page);

                // After login, wait for frameset again
                try {
                    await page.waitForSelector('frameset, frame[name="optop"]', { timeout: 30000 });
                    this.log('OPAL frameset loaded after login', 'info');
                } catch (e2) {
                    throw new Error('Login succeeded but frameset still not found. Page structure may have changed.');
                }
            } else {
                throw new Error('Could not find OPAL frameset or login form');
            }
        }

        return page;
    }

    /**
     * Navigate to "Neuer Auftrag" (New Order) form
     */
    async navigateToNeuerAuftrag(page) {
        this.log('Navigating to Neuer Auftrag form...', 'info');

        // Find header frame (optop)
        let headerFrame = null;
        for (let attempt = 0; attempt < 20; attempt++) {
            const frames = page.frames();
            for (const frame of frames) {
                const frameName = await frame.name();
                if (frameName === 'optop') {
                    headerFrame = frame;
                    this.log('Found header frame (optop)', 'debug');
                    break;
                }
            }
            if (headerFrame) break;
            await page.waitForTimeout(500);
        }

        if (!headerFrame) {
            throw new Error('Could not find header frame (optop)');
        }

        // Wait for navigation links
        await headerFrame.waitForSelector('a', { timeout: 10000 });

        // Try to find and click "Neuer Auftrag" link
        const selectors = [
            'a:has-text("Neuer Auftrag")',
            'a:has-text("neuer auftrag")',
            'a:has-text("Auftrag")',
            'a[href*="new"]',
            'a[href*="auftrag"]',
            'text=Neuer Auftrag'
        ];

        let clicked = false;
        for (const selector of selectors) {
            try {
                await headerFrame.click(selector, { timeout: 5000 });
                this.log(`Clicked on Neuer Auftrag using selector: ${selector}`, 'info');
                clicked = true;
                break;
            } catch (e) {
                this.log(`Selector ${selector} not found, trying next...`, 'debug');
            }
        }

        if (!clicked) {
            throw new Error('Could not find Neuer Auftrag link');
        }

        // Wait for form to load
        await page.waitForTimeout(2000);

        this.log('Successfully navigated to Neuer Auftrag form', 'info');
    }

    /**
     * Find the main content frame (opmain)
     */
    async findMainFrame(page) {
        let mainFrame = null;
        for (let i = 0; i < 10; i++) {
            const frames = page.frames();
            for (const frame of frames) {
                const frameName = await frame.name();
                if (frameName === 'opmain') {
                    mainFrame = frame;
                    this.log('Found main content frame (opmain)', 'debug');
                    break;
                }
            }
            if (mainFrame) break;
            await page.waitForTimeout(500);
        }

        if (!mainFrame) {
            throw new Error('Could not find main content frame (opmain)');
        }

        return mainFrame;
    }

    /**
     * Fill pickup (sender) information - OPAL uses array fields [0] for pickup
     */
    async fillPickupInfo(frame, sender) {
        this.log('Filling pickup (sender) information...', 'info');

        try {
            // OPAL uses arrays for addresses - [0] is pickup, [1] is delivery
            const fieldMappings = [
                { key: 'name', selector: 'input[name="address_name1[]"]', index: 0 },
                { key: 'name2', selector: 'input[name="address_name2[]"]', index: 0 },
                { key: 'contact', selector: 'input[name="address_name3[]"]', index: 0 },
                { key: 'street', selector: 'input[name="address_str[]"]', index: 0 },
                { key: 'houseNumber', selector: 'input[name="address_hsnr[]"]', index: 0 },
                { key: 'country', selector: 'input[name="address_lkz[]"]', index: 0 },
                { key: 'zip', selector: 'input[name="address_plz[]"]', index: 0 },
                { key: 'city', selector: 'input[name="address_ort[]"]', index: 0 },
                { key: 'phoneCountry', selector: 'input[name="address_telefonA[]"]', index: 0 },
                { key: 'phoneArea', selector: 'input[name="address_telefonB[]"]', index: 0 },
                { key: 'phoneNumber', selector: 'input[name="address_telefonC[]"]', index: 0 },
                { key: 'email', selector: 'input[name="address_mail[]"]', index: 0 },
                { key: 'notes', selector: 'textarea[name="address_hinweis[]"]', index: 0 }
            ];

            for (const mapping of fieldMappings) {
                const value = sender[mapping.key];
                if (!value) continue;

                try {
                    const elements = await frame.$$(mapping.selector);
                    if (elements && elements[mapping.index]) {
                        await elements[mapping.index].fill(value.toString());
                        this.log(`  ✓ Filled ${mapping.key}: ${value}`, 'debug');
                    } else {
                        this.log(`  ⚠ Field not found: ${mapping.key}`, 'warn');
                    }
                } catch (e) {
                    this.log(`  ⚠ Error filling ${mapping.key}: ${e.message}`, 'warn');
                }
            }

            this.log('Pickup information filled successfully', 'info');

        } catch (error) {
            throw new Error(`Failed to fill pickup info: ${error.message}`);
        }
    }

    /**
     * Fill delivery (recipient) information - OPAL uses array fields [1] for delivery
     */
    async fillDeliveryInfo(frame, recipient) {
        this.log('Filling delivery (recipient) information...', 'info');

        try {
            // OPAL uses arrays for addresses - [0] is pickup, [1] is delivery
            const fieldMappings = [
                { key: 'name', selector: 'input[name="address_name1[]"]', index: 1 },
                { key: 'name2', selector: 'input[name="address_name2[]"]', index: 1 },
                { key: 'contact', selector: 'input[name="address_name3[]"]', index: 1 },
                { key: 'street', selector: 'input[name="address_str[]"]', index: 1 },
                { key: 'houseNumber', selector: 'input[name="address_hsnr[]"]', index: 1 },
                { key: 'country', selector: 'input[name="address_lkz[]"]', index: 1 },
                { key: 'zip', selector: 'input[name="address_plz[]"]', index: 1 },
                { key: 'city', selector: 'input[name="address_ort[]"]', index: 1 },
                { key: 'phoneCountry', selector: 'input[name="address_telefonA[]"]', index: 1 },
                { key: 'phoneArea', selector: 'input[name="address_telefonB[]"]', index: 1 },
                { key: 'phoneNumber', selector: 'input[name="address_telefonC[]"]', index: 1 },
                { key: 'email', selector: 'input[name="address_mail[]"]', index: 1 },
                { key: 'notes', selector: 'textarea[name="address_hinweis[]"]', index: 1 }
            ];

            for (const mapping of fieldMappings) {
                const value = recipient[mapping.key];
                if (!value) continue;

                try {
                    const elements = await frame.$$(mapping.selector);
                    if (elements && elements[mapping.index]) {
                        await elements[mapping.index].fill(value.toString());
                        this.log(`  ✓ Filled ${mapping.key}: ${value}`, 'debug');
                    } else {
                        this.log(`  ⚠ Field not found: ${mapping.key}`, 'warn');
                    }
                } catch (e) {
                    this.log(`  ⚠ Error filling ${mapping.key}: ${e.message}`, 'warn');
                }
            }

            this.log('Delivery information filled successfully', 'info');

        } catch (error) {
            throw new Error(`Failed to fill delivery info: ${error.message}`);
        }
    }

    /**
     * Fill shipment details (dates, times, package info)
     */
    async fillShipmentDetails(frame, packageInfo, options = {}) {
        this.log('Filling shipment details...', 'info');

        try {
            // Fill order type (Auftragsart)
            if (options.orderType) {
                try {
                    await frame.selectOption('select#seordertype', options.orderType);
                    this.log(`  ✓ Selected order type: ${options.orderType}`, 'debug');
                } catch (e) {
                    this.log('  ⚠ Could not select order type', 'warn');
                }
            }

            // Fill vehicle type (Fahrzeug)
            if (options.vehicleType) {
                try {
                    await frame.selectOption('select#sefztype', options.vehicleType);
                    this.log(`  ✓ Selected vehicle type: ${options.vehicleType}`, 'debug');
                } catch (e) {
                    this.log('  ⚠ Could not select vehicle type', 'warn');
                }
            }

            // Fill dates and times using OPAL array fields
            const dateTimeFields = [
                { key: 'pickupDate', selector: 'input[name="address_date[]"]', index: 0 },
                { key: 'pickupTimeFrom', selector: 'input[name="address_time_von[]"]', index: 0 },
                { key: 'pickupTimeTo', selector: 'input[name="address_time_bis[]"]', index: 0 },
                { key: 'deliveryDate', selector: 'input[name="address_date[]"]', index: 1 },
                { key: 'deliveryTimeFrom', selector: 'input[name="address_time_von[]"]', index: 1 },
                { key: 'deliveryTimeTo', selector: 'input[name="address_time_bis[]"]', index: 1 }
            ];

            for (const field of dateTimeFields) {
                if (!options[field.key]) continue;
                try {
                    const elements = await frame.$$(field.selector);
                    if (elements && elements[field.index]) {
                        await elements[field.index].fill(options[field.key]);
                        this.log(`  ✓ Filled ${field.key}: ${options[field.key]}`, 'debug');
                    }
                } catch (e) {
                    this.log(`  ⚠ Error filling ${field.key}`, 'warn');
                }
            }

            // Fill package information
            const shipmentFields = [
                { key: 'count', selector: 'input#sepksnr' },
                { key: 'weight', selector: 'input#segewicht' },
                { key: 'description', selector: 'input#seinhalt' },
                { key: 'value', selector: 'input#sewert' }
            ];

            for (const field of shipmentFields) {
                if (!packageInfo[field.key]) continue;
                try {
                    await frame.fill(field.selector, packageInfo[field.key].toString());
                    this.log(`  ✓ Filled ${field.key}: ${packageInfo[field.key]}`, 'debug');
                } catch (e) {
                    this.log(`  ⚠ Could not fill ${field.key}`, 'warn');
                }
            }

            // Fill reference number and notes from options
            if (options.refNumber) {
                try {
                    await frame.fill('input#seclref', options.refNumber.toString());
                    this.log(`  ✓ Filled refNumber: ${options.refNumber}`, 'debug');
                } catch (e) {
                    this.log('  ⚠ Could not fill refNumber', 'warn');
                }
            }

            if (options.notes) {
                try {
                    await frame.fill('input#sehinweis', options.notes.toString());
                    this.log(`  ✓ Filled notes: ${options.notes}`, 'debug');
                } catch (e) {
                    this.log('  ⚠ Could not fill notes', 'warn');
                }
            }

            // Fill currency if specified
            if (packageInfo.valueCurrency) {
                try {
                    await frame.selectOption('select#sewertcu', packageInfo.valueCurrency);
                    this.log(`  ✓ Selected currency: ${packageInfo.valueCurrency}`, 'debug');
                } catch (e) {
                    this.log('  ⚠ Could not select currency', 'warn');
                }
            }

            this.log('Shipment details filled successfully', 'info');

        } catch (error) {
            throw new Error(`Failed to fill shipment details: ${error.message}`);
        }
    }

    /**
     * Validate shipment data
     */
    validateShipmentData(shipmentData) {
        const { sender, recipient, package: pkg } = shipmentData;

        // Only delivery address is required - pickup address is optional (OPAL can auto-fill)
        const required = ['name', 'street', 'zip', 'city'];
        const missing = required.filter(field => !recipient[field]);

        if (missing.length > 0) {
            throw new Error(`Missing required recipient fields: ${missing.join(', ')}`);
        }

        this.log('Shipment data validation passed', 'debug');
    }

    /**
     * Create a shipment
     * @param {Object} shipmentData - { sender, recipient, package, options }
     * @returns {Promise<Object>} - { status, trackingNumber, labelUrl, message, internalRef }
     */
    async createShipment(shipmentData) {
        let browser, context, page;
        let error = null;

        try {
            // Normalize data from various formats
            const normalizedData = this._normalizeShipmentData(shipmentData);

            // Validate shipment data
            this.validateShipmentData(normalizedData);

            const { sender = {}, recipient, package: pkg, options = {} } = normalizedData;

            // Initialize browser
            ({ browser, context, page } = await this.initializeBrowser());

            // Navigate to OPAL
            await this.navigateToOpal(page);

            // Navigate to "Neuer Auftrag" form
            await this.navigateToNeuerAuftrag(page);

            // Find main content frame
            const mainFrame = await this.findMainFrame(page);

            // Fill form sections
            if (Object.keys(sender).length > 0) {
                await this.fillPickupInfo(mainFrame, sender);
            }
            await this.fillDeliveryInfo(mainFrame, recipient);
            await this.fillShipmentDetails(mainFrame, pkg, options);

            this.log('Order form filled successfully!', 'info');
            this.log('IMPORTANT: Form is NOT submitted. Please review and submit manually.', 'warn');

            // If headless mode, close browser and return success
            // If not headless, keep it open for manual review
            if (this.headless) {
                await browser.close();
                this.log('Browser closed (headless mode)', 'debug');
            } else {
                this.log('Browser will remain open for manual review.', 'info');
                this.log('Press Ctrl+C to close the browser when done reviewing.', 'info');
                // Wait indefinitely so user can review the form
                await new Promise(() => {});
            }

            return {
                status: 'form_filled',
                trackingNumber: null,
                labelUrl: null,
                message: 'Order form filled successfully. Manual submission required.',
                internalRef: options.refNumber || null,
                provider: 'opal'
            };

        } catch (err) {
            error = err;
            this.log(`Shipment creation failed: ${error.message}`, 'error');

            if (browser) {
                await browser.close();
                this.log('Browser closed due to error', 'debug');
            }

            return {
                status: 'error',
                trackingNumber: null,
                labelUrl: null,
                message: error.message,
                internalRef: null,
                provider: 'opal'
            };
        }
    }

    /**
     * Get tracking status (not implemented for OPAL)
     */
    async getTrackingStatus(trackingNumber) {
        throw new Error('Tracking status not implemented for OPAL driver');
    }
}

module.exports = OpalDriver;
