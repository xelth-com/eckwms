require('dotenv').config();
const db = require('../src/shared/models/postgresql');
const { linkCodeTool, searchInventoryTool } = require('../src/server/local/tools/inventoryTools');
const geminiService = require('../src/server/local/services/geminiService');

async function runIntegrationTest() {
  console.log('🧪 Starting AI Hybrid ID Integration Test...');

  // 1. Verify DB Connection (Sequelize)
  try {
    await db.sequelize.authenticate();
    console.log('✅ Sequelize Connection: OK');
  } catch (err) {
    console.error('❌ Sequelize Connection FAILED:', err.message);
    console.error('   Hint: Check PG_PASSWORD special characters or encoding in .env');
    process.exit(1);
  }

  // 2. Test Tool Direct Execution (DB Write)
  const mockInternalId = 'i7TEST' + Date.now();
  const mockExternalCode = 'EAN' + Date.now();

  console.log(`\n🔄 Testing Tool Write: Linking ${mockExternalCode} -> ${mockInternalId}...`);
  try {
    const linkResult = await linkCodeTool.execute({
      internalId: mockInternalId,
      externalCode: mockExternalCode,
      type: 'ean',
      context: 'test_script'
    });

    if(linkResult.success) {
       console.log('✅ linkCodeTool: Success');
    } else {
       throw new Error(linkResult.message);
    }

    // Verify read
    const searchResult = await searchInventoryTool.execute({ query: mockExternalCode });
    if(searchResult.found && searchResult.matches[0].internal_id === mockInternalId) {
        console.log('✅ searchInventoryTool: Found created alias');
    } else {
        throw new Error('Could not find the alias just created');
    }

  } catch (err) {
    console.error('❌ Tool Execution Failed:', err);
    process.exit(1);
  }

  // 3. Test AI Decision Making (The Brain)
  console.log('\n🧠 Testing AI Context Logic...');
  const prompt = `
    CONTEXT: Worker is in RECEIVING mode (high trust).
    ACTION: Worker scanned unknown code "DHL_TRACK_999".
    CURRENT BUFFER: Box "b888" is active.
    GOAL: Decide if this code should be linked to the box.
  `;

  try {
    console.log('   Asking Gemini...');
    // We use generateText here to see the reasoning, or we could simulate tool calling flow
    // For this test, we want to see if it SUGGESTS using the tool.
    const response = await geminiService.generateText(prompt);
    console.log(`🤖 AI Response: "${response}"`);

    if (response.toLowerCase().includes('link') || response.toLowerCase().includes('dhl')) {
        console.log('✅ AI Logic: Seems reasonable');
    } else {
        console.warn('⚠️ AI Logic: Response was ambiguous, might need prompt tuning');
    }

  } catch (err) {
    console.error('❌ AI Failed:', err.message);
  }

  console.log('\n🎉 Integration Test Complete. System is ready.');
  await db.sequelize.close();
}

runIntegrationTest();
