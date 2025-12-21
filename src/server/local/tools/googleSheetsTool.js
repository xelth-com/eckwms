const googleSheetsService = require('../services/googleSheetsService');

/**
 * Tool for AI to interact with Google Sheets.
 * This allows the AI agent to decide when and what to log.
 */
const googleSheetsTool = {
    name: 'google_sheets_tool',
    description: 'Appends data to a Google Sheet. Use this to log important events, inventory changes, or flagged items.',
    parameters: {
        type: 'object',
        properties: {
            action: {
                type: 'string',
                enum: ['append_row'],
                description: 'The action to perform. Currently only supports "append_row".'
            },
            data: {
                type: 'array',
                items: {
                    type: 'string'
                },
                description: 'An array of strings representing the row values to append (e.g. ["Barcode", "Status", "Note"]).'
            }
        },
        required: ['action', 'data']
    },
    execute: async ({ action, data }) => {
        if (action === 'append_row') {
            try {
                await googleSheetsService.appendToSheet(data);
                return { success: true, message: 'Row appended successfully' };
            } catch (error) {
                return { success: false, error: error.message };
            }
        }
        return { success: false, error: 'Unknown action' };
    }
};

module.exports = googleSheetsTool;
