const { v4: uuidv4 } = require('uuid');

console.log('\nGenerated a unique instance ID. Please add this to your .env file:');
console.log('================================================================');
console.log(`INSTANCE_ID=${uuidv4()}`);
console.log('================================================================\n');
