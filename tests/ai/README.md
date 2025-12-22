# AI Hybrid Identification Tests

This directory contains test scripts for the AI-powered Hybrid Identification system.

## Test Scripts

### 1. `test-ai-response-parsing.js`
**Purpose:** Tests the AI response parsing logic without requiring a database or API connection.

**What it tests:**
- Parsing of different AI interaction types (question, action_taken, confirmation, info)
- Detection of keywords that determine interaction type
- Summary generation from AI responses
- Extraction of suggested actions

**Run:**
```bash
node tests/ai/test-ai-response-parsing.js
```

**Expected Output:**
- 4 test scenarios showing parsed AI interactions
- JSON structure for each interaction type

---

### 2. `test-ai-aliasing.js`
**Purpose:** Full end-to-end integration test of the AI system with database and Gemini API.

**What it tests:**
- Sequelize database connection
- ProductAlias model CRUD operations
- `linkCodeTool` execution (writing to database)
- `searchInventoryTool` execution (reading from database)
- Gemini API connectivity
- AI decision-making logic

**Requirements:**
- PostgreSQL database must be running
- Valid `GEMINI_API_KEY` in `.env`
- Database migrated with `005-add-product-aliases.sql`

**Run:**
```bash
node tests/ai/test-ai-aliasing.js
```

**Expected Output:**
```
🧪 Starting AI Hybrid ID Integration Test...
✅ Sequelize Connection: OK
✅ linkCodeTool: Success
✅ searchInventoryTool: Found created alias
✅ AI Logic: Seems reasonable
🎉 Integration Test Complete. System is ready.
```

---

### 3. `test-gemini-integration.js`
**Purpose:** Basic Gemini API connectivity test.

**What it tests:**
- Gemini API key validity
- GeminiService initialization
- Simple text generation

**Requirements:**
- Valid `GEMINI_API_KEY` in `.env`

**Run:**
```bash
node tests/ai/test-gemini-integration.js
```

---

### 4. `test-feedback-loop.js`
**Purpose:** End-to-end test for the AI Feedback Loop feature (user response handling).

**What it tests:**
- Server health check
- `/API/AI/RESPOND` endpoint functionality
- AI processing of user responses ("yes"/"no")
- Tool execution based on user confirmation
- Request/response structure validation
- Device authentication (if applicable)

**Requirements:**
- Server must be running (`npm start`)
- Valid `GEMINI_API_KEY` in `.env` (for full AI functionality)
- Valid API key for authentication

**Run:**
```bash
# Basic usage (requires server running)
node tests/ai/test-feedback-loop.js

# With custom configuration
TEST_BARCODE="CUSTOM123" TEST_RESPONSE="yes" node tests/ai/test-feedback-loop.js

# Test "no" response
TEST_RESPONSE="no" node tests/ai/test-feedback-loop.js

# Against different server
TEST_SERVER_URL="http://192.168.1.100:3100" node tests/ai/test-feedback-loop.js
```

**Expected Output:**
```
======================================================================
AI Feedback Loop - End-to-End Test
======================================================================
[0] Performing server health check
✓ Server is healthy at http://localhost:3100
[1] Simulating initial scan with unknown barcode
✓ Initial scan simulation completed
[2] Sending user response to /API/AI/RESPOND endpoint
✓ AI response endpoint returned success
✓ AI executed tools based on user response
[3] Verifying database state (conceptual)
✓ Response type indicates AI processed the feedback
======================================================================
Test Summary
======================================================================
✓ Tests passed: 4
✓ All tests passed!
```

**Environment Variables:**
- `TEST_SERVER_URL` - Server URL (default: http://localhost:3100)
- `TEST_API_KEY` - API key for auth (default: public-demo-key-for-eckwms-app)
- `TEST_BARCODE` - Barcode to test (default: TEST12345)
- `TEST_RESPONSE` - User response (default: yes)

---

## Running All Tests

```bash
# Run all AI tests sequentially
for test in tests/ai/test-*.js; do
  echo "Running $test..."
  node "$test"
  echo ""
done
```

## Common Issues

### Database Connection Error
```
ConnectionError: SASL: SCRAM-SERVER-FIRST-MESSAGE: client password must be a string
```
**Fix:** Check `PG_PASSWORD` in `.env`. Ensure special characters are properly escaped.

### API Key Error
```
PERMISSION_DENIED: Your API key was reported as leaked
```
**Fix:** Generate new API key at https://aistudio.google.com/app/apikey

### Migration Not Run
```
relation "product_aliases" does not exist
```
**Fix:** Run migration first:
```bash
node scripts/run-migration.js 005-add-product-aliases.sql
```

---

## Test Data Cleanup

The integration test creates test aliases in the database. To clean them up:

```sql
DELETE FROM product_aliases WHERE created_context = 'test_script';
```

Or programmatically:

```bash
node -e "const db = require('./src/shared/models/postgresql'); \
  db.ProductAlias.destroy({ where: { created_context: 'test_script' } }) \
  .then(count => console.log('Deleted', count, 'test aliases')) \
  .finally(() => db.sequelize.close());"
```
