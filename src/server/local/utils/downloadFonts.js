const fs = require('fs');
const path = require('path');
const https = require('https');

const FONTS_DIR = path.join(__dirname, '../fonts');
if (!fs.existsSync(FONTS_DIR)) {
    fs.mkdirSync(FONTS_DIR, { recursive: true });
}

const fontUrl = 'https://github.com/google/fonts/raw/master/apache/roboto/static/Roboto-Bold.ttf';
const dest = path.join(FONTS_DIR, 'Roboto-Bold.ttf');

console.log('Downloading Roboto-Bold.ttf...');
const file = fs.createWriteStream(dest);
https.get(fontUrl, function(response) {
    if (response.statusCode !== 200) {
        console.error('Failed to download font. Status Code:', response.statusCode);
        return;
    }
    response.pipe(file);
    file.on('finish', function() {
        file.close(() => console.log('✅ Roboto-Bold.ttf downloaded successfully to', dest));
    });
}).on('error', function(err) {
    fs.unlink(dest, () => {});
    console.error('Error downloading font:', err.message);
});
