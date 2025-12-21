const { google } = require('googleapis');
const path = require('path');
const fs = require('fs');

// Path to your service account credentials file
// Expected to be in the project root
const CREDENTIALS_PATH = path.join(__dirname, '../../../../google-credentials.json');

// The ID of the spreadsheet you want to write to
const SPREADSHEET_ID = process.env.GOOGLE_SPREADSHEET_ID;

class GoogleSheetsService {
    constructor() {
        // Lazy initialization to allow server to start without credentials
        this.sheets = null;
        this.initialized = false;
    }

    async init() {
        if (this.initialized) return;

        if (!fs.existsSync(CREDENTIALS_PATH)) {
            console.warn('[GoogleSheets] google-credentials.json not found. Integration disabled.');
            this.initialized = true;
            return;
        }

        try {
            const auth = new google.auth.GoogleAuth({
                keyFile: CREDENTIALS_PATH,
                scopes: ['https://www.googleapis.com/auth/spreadsheets'],
            });
            this.sheets = google.sheets({ version: 'v4', auth });
            this.initialized = true;
            console.log('[GoogleSheets] Service initialized successfully');
        } catch (error) {
            console.warn('[GoogleSheets] Failed to initialize:', error.message);
        }
    }

    /**
     * Appends a row of data to the spreadsheet.
     * @param {Array<string>} values - An array of strings representing the row data.
     * @returns {Promise<Object|null>}
     */
    async appendToSheet(values) {
        if (!SPREADSHEET_ID) {
            // Silent fail if not configured, to not spam logs
            return null;
        }

        await this.init();
        if (!this.sheets) return null;

        try {
            const resource = {
                values: [values],
            };

            const result = await this.sheets.spreadsheets.values.append({
                spreadsheetId: SPREADSHEET_ID,
                range: 'Sheet1!A1', // Appends to the first sheet
                valueInputOption: 'USER_ENTERED',
                resource,
            });

            console.log(`[GoogleSheets] Exported row: ${values[0]}...`);
            return result;
        } catch (err) {
            console.error('[GoogleSheets] Error appending to sheet:', err.message);
            return null;
        }
    }
}

module.exports = new GoogleSheetsService();
