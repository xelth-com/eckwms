require('dotenv').config();
const { EckwmsInstance } = require('../models/postgresql');

async function seedPublicInstance() {
  try {
    const publicKey = 'public-demo-key-for-eckwms-app';
    const [instance, created] = await EckwmsInstance.findOrCreate({
      where: { name: 'Public Demo Account' },
      defaults: {
        server_url: 'https://pda.repair/eckwms/api/scan',
        api_key: publicKey,
        tier: 'free'
      }
    });

    if (created) {
      console.log('Public Demo Account created successfully with API key:', publicKey);
    } else {
      console.log('Public Demo Account already exists.');
      // Ensure API key is correct in case it was changed manually
      if (instance.api_key !== publicKey) {
        instance.api_key = publicKey;
        await instance.save();
        console.log('Updated Public Demo Account with correct API key.');
      }
    }
  } catch (error) {
    console.error('Error seeding public instance:', error);
  } finally {
    process.exit();
  }
}

seedPublicInstance();
