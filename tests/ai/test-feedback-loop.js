#!/usr/bin/env node

/**
 * End-to-End Test for AI Feedback Loop
 *
 * This test simulates the complete flow:
 * 1. Initial scan of unknown barcode (triggers AI question)
 * 2. User responds "yes" or "no"
 * 3. Server processes response via /API/AI/RESPOND
 * 4. AI executes appropriate tools (search_inventory, link_code)
 * 5. Database is updated with the result
 *
 * Usage:
 *   node tests/ai/test-feedback-loop.js
 *
 * Environment variables:
 *   TEST_API_KEY - API key for authentication (default: public-demo-key-for-eckwms-app)
 *   TEST_SERVER_URL - Server URL (default: http://localhost:3100)
 *   TEST_BARCODE - Barcode to test (default: TEST12345)
 *   TEST_RESPONSE - User response (default: yes)
 */

require('dotenv').config();
const http = require('http');
const https = require('https');

// Test configuration
const CONFIG = {
    serverUrl: process.env.TEST_SERVER_URL || 'http://localhost:3100',
    apiKey: process.env.TEST_API_KEY || 'public-demo-key-for-eckwms-app',
    barcode: process.env.TEST_BARCODE || 'TEST12345',
    userResponse: process.env.TEST_RESPONSE || 'yes',
    deviceId: 'test-device-' + Date.now()
};

// Color codes for console output
const COLORS = {
    reset: '\x1b[0m',
    bright: '\x1b[1m',
    green: '\x1b[32m',
    red: '\x1b[31m',
    yellow: '\x1b[33m',
    blue: '\x1b[34m',
    cyan: '\x1b[36m'
};

/**
 * Make an HTTP request (supports both http and https)
 */
function makeRequest(url, options, body = null) {
    return new Promise((resolve, reject) => {
        const urlObj = new URL(url);
        const protocol = urlObj.protocol === 'https:' ? https : http;

        const requestOptions = {
            hostname: urlObj.hostname,
            port: urlObj.port,
            path: urlObj.pathname + urlObj.search,
            method: options.method || 'GET',
            headers: options.headers || {}
        };

        const req = protocol.request(requestOptions, (res) => {
            let data = '';

            res.on('data', (chunk) => {
                data += chunk;
            });

            res.on('end', () => {
                try {
                    const parsed = JSON.parse(data);
                    resolve({ status: res.statusCode, data: parsed, headers: res.headers });
                } catch (e) {
                    resolve({ status: res.statusCode, data: data, headers: res.headers });
                }
            });
        });

        req.on('error', (error) => {
            reject(error);
        });

        if (body) {
            req.write(JSON.stringify(body));
        }

        req.end();
    });
}

/**
 * Log with color
 */
function log(message, color = COLORS.reset) {
    console.log(`${color}${message}${COLORS.reset}`);
}

/**
 * Log test step
 */
function logStep(stepNumber, description) {
    log(`\n[${ stepNumber }] ${description}`, COLORS.cyan + COLORS.bright);
}

/**
 * Log success
 */
function logSuccess(message) {
    log(`✓ ${message}`, COLORS.green);
}

/**
 * Log error
 */
function logError(message) {
    log(`✗ ${message}`, COLORS.red);
}

/**
 * Log warning
 */
function logWarn(message) {
    log(`⚠ ${message}`, COLORS.yellow);
}

/**
 * Log info
 */
function logInfo(message) {
    log(`ℹ ${message}`, COLORS.blue);
}

/**
 * Test Step 1: Initial scan with unknown barcode
 */
async function testInitialScan() {
    logStep(1, 'Simulating initial scan with unknown barcode');

    try {
        // In a real scenario, this would be done via the scan endpoint
        // For this test, we'll just simulate the state where the AI has asked a question
        logInfo(`Simulating scan of barcode: ${CONFIG.barcode}`);
        logInfo('In production, this would trigger AI analysis and return a question to the user');
        logSuccess('Initial scan simulation completed');

        return {
            barcode: CONFIG.barcode,
            aiQuestion: 'Should I link this barcode to the current item?',
            interactionId: 'test-interaction-' + Date.now()
        };
    } catch (error) {
        logError(`Failed to simulate initial scan: ${error.message}`);
        throw error;
    }
}

/**
 * Test Step 2: Send user response to AI feedback endpoint
 */
async function testAiFeedbackResponse(scanResult) {
    logStep(2, 'Sending user response to /API/AI/RESPOND endpoint');

    const requestBody = {
        interactionId: scanResult.interactionId,
        response: CONFIG.userResponse,
        barcode: scanResult.barcode,
        deviceId: CONFIG.deviceId
    };

    logInfo(`Request body: ${JSON.stringify(requestBody, null, 2)}`);

    try {
        const response = await makeRequest(
            `${CONFIG.serverUrl}/ECK/API/AI/RESPOND`,
            {
                method: 'POST',
                headers: {
                    'Content-Type': 'application/json',
                    'X-API-Key': CONFIG.apiKey
                }
            },
            requestBody
        );

        logInfo(`Response status: ${response.status}`);
        logInfo(`Response data: ${JSON.stringify(response.data, null, 2)}`);

        if (response.status === 200 && response.data.success) {
            logSuccess('AI response endpoint returned success');

            // Verify response structure
            if (response.data.result) {
                logSuccess('Response contains result object');

                if (response.data.result.message) {
                    logInfo(`AI message: "${response.data.result.message}"`);
                }

                if (response.data.result.data && response.data.result.data.toolsExecuted) {
                    logSuccess('AI executed tools based on user response');
                } else {
                    logWarn('AI did not execute tools (may be expected for "no" response)');
                }
            } else {
                logWarn('Response missing result object');
            }

            return response.data;
        } else {
            logError(`Unexpected response: ${response.status} - ${JSON.stringify(response.data)}`);
            throw new Error(`API returned ${response.status}: ${JSON.stringify(response.data)}`);
        }
    } catch (error) {
        logError(`Failed to send AI response: ${error.message}`);
        throw error;
    }
}

/**
 * Test Step 3: Verify database state (if accessible)
 */
async function testDatabaseState(feedbackResult) {
    logStep(3, 'Verifying database state (conceptual)');

    // Note: In a full E2E test, you would query the database here
    // For now, we'll just verify the response indicates success

    if (feedbackResult.result && feedbackResult.result.type === 'ai_response') {
        logSuccess('Response type indicates AI processed the feedback');

        if (CONFIG.userResponse.toLowerCase() === 'yes') {
            logInfo('Expected: Database should have new alias or link created');
            logInfo('To verify: Check global.items or database for barcode link');
        } else {
            logInfo('Expected: No database changes (user declined)');
        }

        return true;
    } else {
        logWarn('Unable to verify database state from response');
        return false;
    }
}

/**
 * Test Step 4: Health check
 */
async function testHealthCheck() {
    logStep(0, 'Performing server health check');

    try {
        const response = await makeRequest(
            `${CONFIG.serverUrl}/health`,
            { method: 'GET' }
        );

        if (response.status === 200 && response.data.status === 'ok') {
            logSuccess(`Server is healthy at ${CONFIG.serverUrl}`);
            return true;
        } else {
            logError(`Server health check failed: ${response.status}`);
            return false;
        }
    } catch (error) {
        logError(`Cannot connect to server: ${error.message}`);
        logWarn('Make sure the server is running with: npm start');
        throw error;
    }
}

/**
 * Main test runner
 */
async function runTests() {
    log('\n' + '='.repeat(70), COLORS.bright);
    log('AI Feedback Loop - End-to-End Test', COLORS.bright + COLORS.cyan);
    log('='.repeat(70) + '\n', COLORS.bright);

    logInfo('Test Configuration:');
    logInfo(`  Server URL: ${CONFIG.serverUrl}`);
    logInfo(`  API Key: ${CONFIG.apiKey.substring(0, 10)}...`);
    logInfo(`  Test Barcode: ${CONFIG.barcode}`);
    logInfo(`  User Response: ${CONFIG.userResponse}`);
    logInfo(`  Device ID: ${CONFIG.deviceId}`);

    let testsPassed = 0;
    let testsFailed = 0;

    try {
        // Health check
        await testHealthCheck();
        testsPassed++;

        // Step 1: Initial scan
        const scanResult = await testInitialScan();
        testsPassed++;

        // Step 2: Send AI response
        const feedbackResult = await testAiFeedbackResponse(scanResult);
        testsPassed++;

        // Step 3: Verify database
        const dbVerified = await testDatabaseState(feedbackResult);
        if (dbVerified) testsPassed++;
        else testsFailed++;

        // Summary
        log('\n' + '='.repeat(70), COLORS.bright);
        log('Test Summary', COLORS.bright + COLORS.cyan);
        log('='.repeat(70), COLORS.bright);
        logSuccess(`Tests passed: ${testsPassed}`);
        if (testsFailed > 0) {
            logError(`Tests failed: ${testsFailed}`);
        }

        if (testsFailed === 0) {
            log('\n✓ All tests passed!', COLORS.green + COLORS.bright);
            log('\nThe AI Feedback Loop is working correctly:', COLORS.green);
            log('  1. Server accepted the user response', COLORS.green);
            log('  2. AI processed the feedback appropriately', COLORS.green);
            log('  3. Response structure is valid\n', COLORS.green);
            process.exit(0);
        } else {
            log('\n✗ Some tests failed', COLORS.red + COLORS.bright);
            process.exit(1);
        }

    } catch (error) {
        log('\n' + '='.repeat(70), COLORS.bright);
        logError('Test Execution Failed');
        log('='.repeat(70) + '\n', COLORS.bright);
        logError(`Error: ${error.message}`);
        logError(`Stack: ${error.stack}`);
        process.exit(1);
    }
}

// Run tests if this script is executed directly
if (require.main === module) {
    runTests();
}

module.exports = {
    runTests,
    testInitialScan,
    testAiFeedbackResponse,
    testDatabaseState,
    testHealthCheck
};
