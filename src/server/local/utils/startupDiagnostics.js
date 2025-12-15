const { exec } = require('child_process');
const { getLocalIpAddresses } = require('./networkUtils');

function executeCommand(command) {
    return new Promise((resolve, reject) => {
        exec(command, (error, stdout, stderr) => {
            if (error) {
                console.warn(`[Diagnostics] Warning: Command '${command}' failed: ${stderr}`);
                resolve(`Command failed: ${stderr}`);
                return;
            }
            resolve(stdout.trim());
        });
    });
}

async function collectAndReportDiagnostics() {
    console.log('[Diagnostics] Collecting network information...');
    const globalServerDomain = new URL(process.env.GLOBAL_SERVER_URL || 'https://pda.repair').hostname;

    const [localIps, traceroute] = await Promise.all([
        getLocalIpAddresses(),
        executeCommand(`traceroute ${globalServerDomain}`)
    ]);

    const port = process.env.LOCAL_SERVER_PORT || process.env.PORT || 3100;
    const diagnosticsPayload = {
        instanceId: process.env.INSTANCE_ID,
        serverPublicKey: process.env.SERVER_PUBLIC_KEY,
        localIps: localIps,
        tracerouteToGlobal: traceroute,
        port: port
    };

    console.log('[Diagnostics] Payload collected:', JSON.stringify(diagnosticsPayload, null, 2));

    const registrationUrl = process.env.GLOBAL_SERVER_REGISTER_URL;
    if (!registrationUrl) {
        console.error('[Diagnostics] ERROR: GLOBAL_SERVER_REGISTER_URL is not set in .env. Cannot register instance.');
        return;
    }

    try {
        console.log(`[Diagnostics] Registering instance with global server at ${registrationUrl}...`);
        const response = await fetch(registrationUrl, {
            method: 'POST',
            headers: {
                'Content-Type': 'application/json',
                'X-Internal-Api-Key': process.env.GLOBAL_SERVER_API_KEY
            },
            body: JSON.stringify(diagnosticsPayload)
        });

        if (!response.ok) {
            throw new Error(`Global server responded with status ${response.status}`);
        }

        const responseData = await response.json();
        console.log('[Diagnostics] Successfully registered with global server. Detected public IP:', responseData.detectedIp);
    } catch (error) {
        console.error('[Diagnostics] FAILED to register with global server:', error.message);
    }
}

module.exports = { collectAndReportDiagnostics };
