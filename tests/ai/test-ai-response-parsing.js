// Test script to demonstrate AI response parsing
// This simulates what the client will receive

/**
 * Parse AI response and determine interaction type
 * (Copied from scanHandler for demonstration)
 */
function parseAIResponse(aiText, iterations = 0) {
    const interaction = {
        type: 'info',
        message: aiText,
        requiresResponse: false,
        suggestedActions: [],
        toolCallsMade: iterations
    };

    // Detect if AI is asking a question
    const hasQuestion = aiText.includes('?');
    const questionKeywords = ['should i', 'do you want', 'would you like', 'is this', 'are you'];
    const isAsking = questionKeywords.some(keyword => aiText.toLowerCase().includes(keyword));

    if (hasQuestion && isAsking) {
        interaction.type = 'question';
        interaction.requiresResponse = true;
        interaction.suggestedActions = ['yes', 'no', 'cancel'];
    }

    // Detect if AI took action
    const actionKeywords = ['linked', 'created', 'associated', 'added'];
    const tookAction = actionKeywords.some(keyword => aiText.toLowerCase().includes(keyword));

    if (tookAction && iterations > 0) {
        interaction.type = 'action_taken';
        interaction.requiresResponse = false;
    }

    // Detect if AI needs confirmation
    const confirmKeywords = ['confirm', 'verify', 'check', 'make sure'];
    const needsConfirm = confirmKeywords.some(keyword => aiText.toLowerCase().includes(keyword));

    if (needsConfirm && hasQuestion) {
        interaction.type = 'confirmation';
        interaction.requiresResponse = true;
        interaction.suggestedActions = ['confirm', 'cancel'];
    }

    // Extract short summary
    const firstSentence = aiText.split(/[.!?]/)[0];
    interaction.summary = firstSentence.length > 100
        ? firstSentence.substring(0, 97) + '...'
        : firstSentence;

    return interaction;
}

// Test scenarios
console.log('🧪 Testing AI Response Parsing\n');

// Scenario 1: AI asks a question
const scenario1 = parseAIResponse(
    "I don't recognize 'ABC123'. Is this a new product code, or did you mean to scan something else?",
    0
);
console.log('📋 Scenario 1: AI asks for clarification');
console.log(JSON.stringify(scenario1, null, 2));
console.log('');

// Scenario 2: AI took action
const scenario2 = parseAIResponse(
    "Linked EAN 9780123456789 to i7abc123. This is now associated with the current item.",
    2
);
console.log('📋 Scenario 2: AI took action (linked code)');
console.log(JSON.stringify(scenario2, null, 2));
console.log('');

// Scenario 3: AI needs confirmation
const scenario3 = parseAIResponse(
    "This appears to be a DHL tracking number. Do you want to confirm linking it to box b888?",
    1
);
console.log('📋 Scenario 3: AI needs confirmation');
console.log(JSON.stringify(scenario3, null, 2));
console.log('');

// Scenario 4: AI provides info
const scenario4 = parseAIResponse(
    "This code format doesn't match any known patterns. It might be a custom internal code.",
    0
);
console.log('📋 Scenario 4: AI provides information');
console.log(JSON.stringify(scenario4, null, 2));
console.log('');

console.log('✅ All scenarios tested. Client can now render appropriate UI based on interaction.type');
