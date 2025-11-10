require('dotenv').config();
const nacl = require('tweetnacl');
const { Buffer } = require('node:buffer');

console.log('Generating new server key pair...');

const keyPair = nacl.sign.keyPair();

const publicKeyBase64 = Buffer.from(keyPair.publicKey).toString('base64');
const privateKeyBase64 = Buffer.from(keyPair.secretKey).toString('base64');

console.log('\nSuccessfully generated keys. Please add the following lines to your .env file:');
console.log('================================================================');
console.log(`SERVER_PUBLIC_KEY=${publicKeyBase64}`)
console.log(`SERVER_PRIVATE_KEY=${privateKeyBase64}`)
console.log('================================================================\n');
