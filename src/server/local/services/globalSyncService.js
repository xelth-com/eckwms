require('dotenv').config();
const GLOBAL_SERVER_API_ENDPOINT = process.env.GLOBAL_SERVER_API_ENDPOINT;
const GLOBAL_SERVER_API_KEY = process.env.GLOBAL_SERVER_API_KEY;

async function syncPublicData(data) {
  if (!GLOBAL_SERVER_API_ENDPOINT || !GLOBAL_SERVER_API_KEY) {
    console.warn('[SyncService] GLOBAL_SERVER_API_ENDPOINT or GLOBAL_SERVER_API_KEY not configured. Remote sync disabled.');
    return;
  }
  try {
    console.log(`[SyncService] Pushing data for ID: ${data.id} to ${GLOBAL_SERVER_API_ENDPOINT}`);
    const response = await fetch(GLOBAL_SERVER_API_ENDPOINT, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'X-Internal-Api-Key': GLOBAL_SERVER_API_KEY
      },
      body: JSON.stringify(data)
    });
    if (!response.ok) {
      const errorBody = await response.text();
      throw new Error(`Sync failed with status ${response.status}: ${errorBody}`);
    }
    console.log(`[SyncService] Successfully synced data for ID: ${data.id}`);
  } catch (error) {
    console.error('[SyncService] Error syncing public data:', error.message);
  }
}

module.exports = { syncPublicData };
