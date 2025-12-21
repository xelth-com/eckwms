require('dotenv').config();
const geminiService = require('../src/server/local/services/geminiService');
const googleSheetsTool = require('../src/server/local/tools/googleSheetsTool');

async function runTest() {
  console.log('🧪 Starting Gemini Integration Smoke Test...');

  // 1. Check Environment
  if (!process.env.GEMINI_API_KEY) {
    console.error('❌ GEMINI_API_KEY is missing in .env');
    process.exit(1);
  }
  console.log('✅ GEMINI_API_KEY found');

  // 2. Test Text Generation
  try {
    console.log('🔄 Testing Gemini Text Generation...');
    const response = await geminiService.generateText('Hello! Respond with "OK" if you can hear me.');
    console.log(`🤖 AI Response: "${response}"`);
    if (response.includes('OK') || response.length > 0) {
        console.log('✅ Text Generation working');
    } else {
        console.warn('⚠️ Response received but unexpected content');
    }
  } catch (error) {
    console.error('❌ Text Generation Failed:', error.message);
  }

  // 3. Test Google Sheets Tool Structure
  console.log('🔄 Testing Tool Definition...');
  if (googleSheetsTool.name === 'google_sheets_tool' && typeof googleSheetsTool.execute === 'function') {
      console.log('✅ Google Sheets Tool is correctly defined');
  } else {
      console.error('❌ Google Sheets Tool malformed');
  }

  console.log('\n🏁 Test Sequence Complete');
}

runTest();
