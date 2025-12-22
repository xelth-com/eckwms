const fs = require('fs');
const path = require('path');
const https = require('https');

const FONTS_DIR = path.join(__dirname, '../src/server/local/fonts');

if (!fs.existsSync(FONTS_DIR)) {
    fs.mkdirSync(FONTS_DIR, { recursive: true });
    console.log(`Created fonts directory: ${FONTS_DIR}`);
}

// Using direct download from a reliable source
const fonts = [
    { name: 'Roboto-Regular.ttf', url: 'https://raw.githubusercontent.com/googlefonts/roboto/main/src/hinted/Roboto-Regular.ttf' },
    { name: 'Roboto-Medium.ttf', url: 'https://raw.githubusercontent.com/googlefonts/roboto/main/src/hinted/Roboto-Medium.ttf' },
    { name: 'Roboto-Italic.ttf', url: 'https://raw.githubusercontent.com/googlefonts/roboto/main/src/hinted/Roboto-Italic.ttf' },
    { name: 'Roboto-MediumItalic.ttf', url: 'https://raw.githubusercontent.com/googlefonts/roboto/main/src/hinted/Roboto-MediumItalic.ttf' }
];

const downloadFile = (url, dest) => {
    return new Promise((resolve, reject) => {
        const file = fs.createWriteStream(dest);

        const makeRequest = (requestUrl) => {
            https.get(requestUrl, (response) => {
                // Handle redirects
                if (response.statusCode === 301 || response.statusCode === 302) {
                    file.close();
                    fs.unlink(dest, () => {});
                    makeRequest(response.headers.location);
                    return;
                }

                if (response.statusCode !== 200) {
                    reject(new Error(`Failed to download ${requestUrl}, status: ${response.statusCode}`));
                    return;
                }

                response.pipe(file);
                file.on('finish', () => {
                    file.close();
                    resolve();
                });
            }).on('error', (err) => {
                fs.unlink(dest, () => {});
                reject(err);
            });
        };

        makeRequest(url);
    });
};

(async () => {
    console.log('Downloading fonts...');
    for (const font of fonts) {
        const dest = path.join(FONTS_DIR, font.name);
        if (fs.existsSync(dest)) {
            console.log(`Skipping ${font.name} (exists)`);
            continue;
        }
        try {
            await downloadFile(font.url, dest);
            console.log(`Downloaded ${font.name}`);
        } catch (e) {
            console.error(`Error downloading ${font.name}:`, e.message);
        }
    }
    console.log('Font installation complete.');
})();
