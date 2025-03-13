require('dotenv').config();
const { Betruger, User, Order, Place, Box, Item, Dict } = require('./models');
const {
    betrugerTimeIvBase32,
    betrugerToBase32,
    betrugerToHex,
    betrugerUrlEncrypt,
    betrugerUrlDecrypt,
    generateJWT,
    verifyJWT,
    betrugerCrc,
    createSecretJwtKey,
    createEncryptionKey,
    base32table
} = require('./utils/encryption');
const serverPort = 3000;
baseDirectory = __dirname + '/';
runOnServer = Object.hasOwn(process.env, 'pm_id');
var serialIi = 999999999999999;
var serialI = 1;
var serialB = 1;
var serialP = 1;

const OpenAI = require("openai");

const openai = new OpenAI({
    apiKey: process.env.OPENAI_API_KEY,
});

dict = new Dict('');
dicts = new Map();

async function translateText(text, targetLang) {
    try {
        // Erstelle eine Chat-Vervollständigung
        const completion = await openai.chat.completions.create({
            model: "gpt-4o-mini",
            store: true, // Daten speichern, falls nötig
            messages: [
                { role: "system", content: `You are a professional translator. Translate this to ${targetLang}.` },
                { role: "user", content: text }
            ]
        });

        // Extrahiere den übersetzten Text aus der Antwort
        return completion.choices[0].message.content.trim();
    } catch (error) {
        console.error("Translation error:", error);
        throw new Error("Failed to translate text.");
    }
}


user = new User('');
order = new Order('');
item = new Item('');
box = new Box('');
place = new Place('');
users = new Map();
orders = new Map();
uppers = new Map();
classes = new Map();
items = new Map();
boxes = new Map();
places = new Map();
orders = new Map();


const http = require('http');
const crc32 = require('buffer-crc32');
const PdfPrinter = require('pdfmake');
const fs = require('fs');

const { SerialPort } = require('serialport')
const {
    randomFill,
    randomFillSync,
    createCipheriv,
    createDecipheriv,
    generateKey,
    createSecretKey,
    createHmac,
    KeyObject
} = require('node:crypto');
const { Buffer } = require('node:buffer');
const { receiveMessageOnPort } = require('node:worker_threads');

const algorithm = 'aes-192-gcm';

const unixTime = () => {
    return Math.floor(Date.now() / 1000);
}

/*
generateKey('aes', { length: 192 }, (err, key) => {
    if (err) throw err;
}); 
generateKey('hmac', { length: 256 }, (err, key) => {
    if (err) throw err;
    console.log(key.export().toString('hex'));  // 46e..........620
});*/








const secretJwt = createSecretKey(process.env.JWT_SECRET, 'hex');
























const betrugerPrintCodesPdf = (codeType, startNumber = 0, arrDim = []) => {
    const fonts = {
        Roboto: {
            normal: 'fonts/Roboto-Regular.ttf'
        }
    };
    const printer = new PdfPrinter(fonts);
    var dd = {
        content: [
            {
                table: {
                    widths: ['*', '*'],
                    heights: 44,
                    body: []
                },
                layout: 'noBorders'
            }]
    };
    const labelMake = (code, field1, field2) => {

        label = {
            alignment: 'center',
            margin: [0, 3, 0, 0],
            columns: [
                { qr: `ECK1.COM/${code}M3`, fit: '29' }
                ,
                {
                    width: 'auto',
                    fontSize: 25,
                    alignment: 'center',
                    text: field1
                },
                { qr: `ECK2.COM/${code}M3`, fit: '29' }
                ,
                {
                    margin: [0, -4, 0, 0],
                    width: 'auto',
                    fontSize: 32,
                    alignment: 'center',
                    text: field2
                },
                { qr: `ECK3.COM/${code}M3`, fit: '29' }
            ],
            columnGap: 1
        };
        return label;
    };
    body = new Array(16).fill(0);
    if (codeType === 'i') {
        body.forEach((element, index) => {
            const index1 = 2 * index + startNumber;
            const index2 = 2 * index + startNumber + 1;
            const codeTemp1 = `${betrugerUrlEncrypt(`${codeType}${('000000000000000000' + (index1)).slice(-18)}`)}`;
            const codeTemp2 = `${betrugerUrlEncrypt(`${codeType}${('000000000000000000' + (index2)).slice(-18)}`)}`;
            const temp1 = crc32.unsigned(index1.toString()) & 1023;
            const field2Temp1 = Buffer.from([base32table[temp1 >> 5], base32table[temp1 & 31]]).toString();
            const temp2 = crc32.unsigned(index2.toString()) & 1023;
            const field2Temp2 = Buffer.from([base32table[temp2 >> 5], base32table[temp2 & 31]]).toString();
            dd.content[0].table.body.push([labelMake(codeTemp1, `${('000000' + index1).slice(-6)}`, field2Temp1), labelMake(codeTemp2, `${('000000' + index2).slice(-6)}`, field2Temp2)]);
        });
    };
    if (codeType === 'b') {
        body.forEach((element, index) => {
            const index1 = index + startNumber;
            const codeTemp1 = `${betrugerUrlEncrypt(`${codeType}${('000000000000000000' + (index1)).slice(-18)}`)}`;
            const temp1 = crc32.unsigned(index1.toString()) & 1023;
            const field2Temp1 = Buffer.from([base32table[temp1 >> 5], base32table[temp1 & 31]]).toString();
            dd.content[0].table.body.push([labelMake(codeTemp1, `#${('00000' + index1).slice(-5)}`, field2Temp1), labelMake(codeTemp1, `#${('00000' + index1).slice(-5)}`, field2Temp1)]);
        });
    };
    if (codeType === 'p') {
        body.forEach((element, index) => {
            const index1 = 2 * index + startNumber - 1;
            const index2 = 2 * index + startNumber + 1 - 1;
            const place00 = (index1) % arrDim[0][1];
            const place01 = ((index1 - place00) / arrDim[0][1]) % arrDim[1][1];
            const place10 = (index2) % arrDim[0][1];
            const place11 = ((index2 - place10) / arrDim[0][1]) % arrDim[1][1];
            const codeTemp1 = `${betrugerUrlEncrypt(`${codeType}${('000000000000000000' + (index1 + 1)).slice(-18)}`)}`;
            const codeTemp2 = `${betrugerUrlEncrypt(`${codeType}${('000000000000000000' + (index2 + 1)).slice(-18)}`)}`;
            dd.content[0].table.body.push([labelMake(codeTemp1, `${arrDim[1][0]}${place01 + 1}`, `${arrDim[0][0]}${place00 + 1}`), labelMake(codeTemp2, `${arrDim[1][0]}${place11 + 1}`, `${arrDim[0][0]}${place10 + 1}`)]);
        });
    };



    var options = {}
    var pdfDoc = printer.createPdfKitDocument(dd, options);
    pdfDoc.pipe(fs.createWriteStream(`eckwms_${codeType}${startNumber}.pdf`));
    pdfDoc.end();

}


//betrugerPrintCodesPdf('p', 30 + 1, [['', 5], ['', 20]]);

//betrugerPrintCodesPdf('i', 32 * 13 + 1, [['', 5], ['', 20]]);

//betrugerPrintCodesPdf('b', 16 * 10 + 1, [['', 5], ['', 20]]);







const { readFile, readdir, appendFile, writeFile, access } = require('node:fs/promises');
const { resolve, join } = require('node:path');


async function writeLog(str) {
    const dateTemp = new Date(Date.now());
    return appendFile(resolve(`./logs/${dateTemp.getUTCFullYear().toString().slice(-2)}${('00' + (dateTemp.getUTCMonth() + 1)).slice(-2)}.txt`), `${str}\t\t\t\t\t${dateTemp.getUTCDate()}_${dateTemp.getUTCHours()}:${dateTemp.getUTCMinutes()}:${dateTemp.getUTCSeconds()}\n`)
}

function writeLargeMapToFile(map, filePath) {
    return new Promise((resolve, reject) => {
        const writeStream = fs.createWriteStream(filePath); // Erstellen eines Write-Streams

        //writeStream.write('[\n'); // Anfang der JSON-Datei

        let firstEntry = true; // Flag für das erste Element

        // Iteriere über die Map und schreibe jedes Element einzeln
        for (const [key, value] of map) {
            if (!firstEntry) {
                writeStream.write('\n'); // Komma zwischen den Einträgen
            } else {
                firstEntry = false; // Setze das Flag nach dem ersten Element auf false
            }

            // Schreibe den Schlüssel-Wert-Paar als JSON
            //if (value.cl !== '') value.cl = Object.getPrototypeOf(value).cl;
            writeStream.write(JSON.stringify(value));
        }

        //writeStream.write('\n]\n'); // Ende der JSON-Datei

        // Beendet den Stream
        writeStream.end();

        // Ereignisbehandlung: `finish` für Erfolg, `error` für Fehler
        writeStream.on('finish', () => {
            resolve(`Datei erfolgreich geschrieben: ${filePath}`);
        });

        writeStream.on('error', (err) => {
            reject(`Fehler beim Schreiben in die Datei: ${err}`);
        });
    });
}









async function logOut(mainDirectory) {

    try {
        await writeLargeMapToFile(users, resolve(`${mainDirectory}base/users.json`));
        await writeLargeMapToFile(orders, resolve(`${mainDirectory}base/orders.json`))
        await writeLargeMapToFile(items, resolve(`${mainDirectory}base/items.json`));
        await writeLargeMapToFile(boxes, resolve(`${mainDirectory}base/boxes.json`));
        await writeLargeMapToFile(places, resolve(`${mainDirectory}base/places.json`));
        await writeLargeMapToFile(classes, resolve(`${mainDirectory}base/classes.json`));
        await writeLargeMapToFile(uppers, resolve(`${mainDirectory}base/uppers.json`));
        await writeLargeMapToFile(dicts, resolve(`${mainDirectory}base/dicts.json`));
        await writeFile(resolve(`${mainDirectory}base/ini.json`), JSON.stringify({ serialIi, serialI, serialB, serialP }));
        await writeLog('logout ' + Object.values(process.memoryUsage()));


    } catch (err) {
        // When a request is aborted - err is an AbortError
        console.error(err);
    }

}


async function readLinesToMap(map, filePath, cla) {
    try {
        // Überprüfen, ob die Datei existiert (asynchron)
        await access(filePath);

        return new Promise((resolve, reject) => {
            const readInterface = readline.createInterface({
                input: fs.createReadStream(filePath), // Erstellen eines Read-Streams für die Datei
                console: false
            });

            readInterface.on('line', (line) => {
                try {
                    const jsonObj = JSON.parse(line);
                    if (Object.hasOwn(jsonObj, 'sn')) {
                        const key = jsonObj.sn[0];
                        if (key !== undefined) {
                            if (jsonObj.cl && classes.has(jsonObj.cl)) {
                                Object.setPrototypeOf(jsonObj, classes.get(jsonObj.cl));
                            } else {
                                Object.setPrototypeOf(jsonObj, cla);
                            }
                            map.set(key, jsonObj);
                        }
                    } else if (Object.hasOwn(jsonObj, 'cl')) {
                        const key = jsonObj.cl;
                        Object.setPrototypeOf(jsonObj, cla);
                        map.set(key, jsonObj);
                    } else if (Object.hasOwn(jsonObj, 'orig')) {
                        const key = jsonObj.orig;
                        Object.setPrototypeOf(jsonObj, cla);
                        map.set(key, jsonObj);
                    }
                } catch (err) {
                    console.error('Fehler beim Verarbeiten der Zeile:', err);
                }
            });

            readInterface.on('close', () => {
                resolve();
            });

            readInterface.on('error', (err) => {
                reject(`Fehler beim Lesen der Datei: ${err.message}`);
            });
        });
    } catch (err) {
        console.log(`Die Datei ${filePath} existiert nicht oder kann nicht geöffnet werden.`);
    }
}



async function classesUpdate() {

    for (const [key, value] of items) {

        if (value.cl && classes.has(value.cl)) {
            addUnicEntryToProperty(classes, value.cl, ['i', value.sn[0]], 'down')
        } else {
            //console.log(value, '\n\n\n\n');


        }
    }

}

async function upperUpdate() {

    for (const [key, value] of classes) {
        value.rel.map(el => {
            if (el[0] == 'partOf') {
                if (!uppers.has(el[1])) {
                    uppers.set(el[1], Object.create(item));
                    uppers.get(el[1]).cl = el[1];
                }
                addUnicEntryToProperty(classes, value.cl, ['u', el[1]], 'up');
                addUnicEntryToProperty(uppers, el[1], ['c', value.cl], 'down');
            }
        });
    }


}

/**
 * Adds UNIC new information to a specific property of an object within a Map.
 *
 * @param {Map} collection - The Map that contains the objects.
 * @param {string} key -  will be uset for searchin in the Map.
 * @param {string|number} inputValue - The user input to be stored as an integer.
 * @param {string} targetProperty - The key of the property to which the information should be added.
 */
function addUnicEntryToProperty(collection, targetKey, inputValue, targetProperty) {
    const targetItem = collection.get(disAct(targetKey));
    if (targetItem) {
        if (!Object.hasOwn(targetItem, targetProperty)) {
            targetItem[targetProperty] = [];
        }

        if (!targetItem[targetProperty].reduce((akk, cur) => akk || (cur[0] === inputValue[0] && cur[1] === inputValue[1]), false)) {
            targetItem[targetProperty].push(inputValue);
        }
    }
}


/**
 * Adds a value to a specific property of an object within a Map.
 * If the property does not exist, it will be initialized as an array.
 * 
 * @param {Map} collection - The Map containing the objects.
 * @param {string|number} targetKey - The key used to search the Map.
 * @param {any} inputValue - The value to be added to the target property.
 * @param {string} targetProperty - The property key where the value should be added.
 */
function addEntryToProperty(collection, targetKey, inputValue, targetProperty) {
    // Check if the collection is a Map
    if (!(collection instanceof Map)) {
        throw new TypeError("The collection parameter must be a Map.");
    }

    // Retrieve the target object from the Map
    const targetItem = collection.get(disAct(targetKey));
    if (!targetItem) {
        console.warn(`Key '${targetKey}' not found in the Map.`);
        return;
    }

    // Ensure the target property exists and is an array
    if (!Object.hasOwn(targetItem, targetProperty)) {
        targetItem[targetProperty] = [];
    } else if (!Array.isArray(targetItem[targetProperty])) {
        throw new TypeError(`Property '${targetProperty}' is not an array on the target object.`);
    }

    // Add the input value to the array
    targetItem[targetProperty].push(inputValue);
}



async function initialisation(mainDirectory) {
    try {
        /* // this part is for making new classes from some data
                const out = [];
                const files = await readdir(resolve(`${mainDirectory}classes/M3/`));
                files.forEach((element, index) => {
                    const filePath = resolve(`${mainDirectory}classes/M3/${element}`);
                    out.push(readFile(filePath, { encoding: 'utf8' }));
                });
        
                (await Promise.all(out)).forEach((prArr) => {
                    JSON.parse(prArr).forEach((classDefenition, index) => {
                        if (classDefenition.pn[0][1] === 'm3' && classDefenition.pn[0][0].length < 15) {
                            Object.setPrototypeOf(classDefenition, item)
                            classDefenition.cl = classDefenition.pn[0][0];
                            classes.set(classDefenition.pn[0][0], classDefenition)
        
                        }
                    });
                });
        */
        await readLinesToMap(users, resolve(`${mainDirectory}base/users.json`), user);
        await readLinesToMap(orders, resolve(`${mainDirectory}base/orders.json`), order);
        await readLinesToMap(uppers, resolve(`${mainDirectory}base/uppers.json`), item);
        await readLinesToMap(classes, resolve(`${mainDirectory}base/classes.json`), item);
        await readLinesToMap(items, resolve(`${mainDirectory}base/items.json`), item);
        await readLinesToMap(boxes, resolve(`${mainDirectory}base/boxes.json`), box);
        await readLinesToMap(places, resolve(`${mainDirectory}base/places.json`), place);
        await readLinesToMap(dicts, resolve(`${mainDirectory}base/dicts.json`), dict);
    } catch (err) {
        console.error(err.message);
    }
}

process.on('SIGINT', function () {

    logOut(baseDirectory).then(() => {

        process.exit(0);
    })
});

const readline = require('node:readline');
const { stdin: input, stdout: output } = require('node:process');
const { setPriority } = require('os');

var iTem = [];
var bOx = [];
var pLace = [];
var aCtion = [];





const rl = readline.createInterface({ input, output });


// Globale Variable für die Speicherung des Inputs
let userInput = "";

// Funktion zum asynchronen Lesen von der Konsole
function readFromConsole(prompt) {
    return new Promise((resolve) => {
        rl.question(prompt, (input) => {
            resolve(input);
        });
    });
}



function fullPath(boxitem) {
    if (boxitem.loc && boxitem.loc.length > 0) {
        let pathArr = [];
        const lastLoc = boxitem.loc.at(-1); // Hol die letzte Position aus loc
        const locSn = lastLoc[0]; // Vollständiger Schlüssel, z. B. "p000000000000000031" oder "b000000000000000001"

        if (locSn.startsWith('b')) {
            const box = boxes.get(locSn); // Suche die Box mit der neuen SN
            if (box) {
                pathArr = fullPath(box); // Rekursion
            }
            pathArr.push(locSn); // Füge die aktuelle Box-SN hinzu
        }

        if (locSn.startsWith('p')) {
            pathArr.push(locSn); // Füge den Platz direkt hinzu
        }

        return pathArr;
    }
    return [];
}

function formatUnixTimestamp(timestamp) {
    const date = new Date(parseInt(timestamp, 10) * 1000);
    const year = date.getUTCFullYear();
    const month = String(date.getUTCMonth() + 1).padStart(2, '0');
    const day = String(date.getUTCDate()).padStart(2, '0');
    const hours = String(date.getUTCHours()).padStart(2, '0');
    const minutes = String(date.getUTCMinutes()).padStart(2, '0');
    const seconds = String(date.getUTCSeconds()).padStart(2, '0');
    return `${year}-${month}-${day} ${hours}:${minutes}:${seconds}`;
}




function prettyPrintObject(obj) {


    const json = JSON.stringify(obj, null, 2);

    const coloredJson = json.replace(
        /("(.*?)": )|("(.*?)")|(\b\d{10}\b)|(\b\d+\b)|\b(true|false|null)\b/g,
        (match, key, keyContent, string, stringContent, unixTimestamp, number, boolNull) => {
            if (key) {
                return `<span style="color: brown;">${key}</span>`; // Schlüssel in Braun
            }
            if (string) {
                return `<span style="color: green;">${string}</span>`; // Strings in Grün
            }
            if (unixTimestamp) {
                const formattedDate = formatUnixTimestamp(unixTimestamp);
                return `<span style="color: purple;">"${formattedDate}"</span>`; // Unixzeit in lesbare Zeit
            }
            if (number) {
                return `<span style="color: orange;">${number}</span>`; // Andere Zahlen in Orange
            }
            if (boolNull) {
                return `<span style="color: blue;">${boolNull}</span>`; // true, false, null in Blau
            }
            return match;
        }
    );

    return `<pre style="background-color: #f4f4f4; padding: 10px; border-radius: 5px; font-family: monospace; white-space: pre-wrap;">${coloredJson}</pre>`;
}



function maskObjectFields(obj, fieldsToMask) {
    function maskString(str) {
        return str
            .split(' ')
            .map(word => (word.length > 1 ? word[0] + '*'.repeat(word.length - 1) : word))
            .join(' ');
    }

    function recursiveMask(obj) {
        const newObj = Array.isArray(obj) ? [] : {};
        for (const key in obj) {
            if (Object.prototype.hasOwnProperty.call(obj, key)) {
                const value = obj[key];
                if (fieldsToMask.includes(key) && typeof value === 'string') {
                    newObj[key] = maskString(value);
                } else if (typeof value === 'object' && value !== null) {
                    newObj[key] = recursiveMask(value);
                } else {
                    newObj[key] = value;
                }
            }
        }
        return newObj;
    }

    return recursiveMask(obj);
}

const fieldsToMask = ["comp", "pers", "str", "cem", "iem"];
// Function to format Unix timestamp to readable date and time
function formatUnixTimestamp(unixTimestamp) {
    const dateObj = new Date(unixTimestamp * 1000);
    return dateObj.toLocaleString('en-US'); // Using English locale
}

// Function to determine font color based on days since registration and return status
function getFontColor(daysSinceRegistration, returned) {
    if (returned) {
        return 'color: green;';
    } else {
        if (daysSinceRegistration <= 0) {
            return 'color: blue;';
        } else if (daysSinceRegistration >= 30) { // Changed from 21 to 30 days
            return 'color: red;';
        } else {
            // Smooth gradient from red to blue
            const ratio = daysSinceRegistration / 30;
            const red = Math.floor(255 * (1 - ratio));
            const blue = Math.floor(255 * ratio);
            return `color: rgb(${red}, 0, ${blue});`;
        }
    }
}



const parseHtml = async (parsed) => {
    try {
        parsed.text = parsed.text.trim();
        htmlContentStart = '';
        htmlContent = '';
        htmlContent2 = '';
        htmlContentEnd = '';
        console.log(parsed, runOnServer)
        if (parsed.jwt) { //parsed.jwt

            try {
                const payload = verifyJWT(parsed.jwt, secretJwt);

                console.log('JWT gültig:', payload);
                if (Object.hasOwn(payload, 'u')) {

                    if (parsed.dest == 'outputShow' && ((tempBet = findKnownCode(parsed.text)) || (tempBet = isBetDirect(parsed.text)))) {
                        const type = tempBet.slice(0, 1);
                        htmlContentStart = ``
                        try {
                            switch (type) {
                                case 'i': htmlContent = prettyPrintObject(items.get(tempBet)); break;
                                case 'b': htmlContent = prettyPrintObject(boxes.get(tempBet)); break;
                                case 'p': htmlContent = prettyPrintObject(places.get(tempBet)); break;
                                case 'o': htmlContent = prettyPrintObject(orders.get(tempBet)); break;
                                case 'u': htmlContent = prettyPrintObject(users.get(tempBet)); break;
                                default: htmlContent = 'Unknown type: ' + tempBet;
                            }
                        } catch {
                            htmlContent = 'nothing found'
                        }
                        htmlContentEnd = ``;
                    } else if (parsed.name == 'startInput') {
                        htmlContentStart = `<div id='outputTable1' class="cellPaper" style="box-shadow: 0px 10px 20px -10px rgba(255, 255, 255, 0.4)">
                        <span><div style="width: 100%; background-color:#BBB; color: white; font-weight: bold; text-align: center; border: none;">Incorrectly filled out RMA requests and other forms</div>
                        <table style="width:100%; word-wrap:break-word;border: none;"> 
                        <tr style="background-color:#BBB"><td style="width:80px;">Box</td><td style="width:120px;">inDate</td><td style="width:80px;">Days</td><td style="width:120px;">outDate</td><td>Customer</td><td>Order</td><td>Contain</td><td style="width:200px;">Shipping</td></tr>
                        `
                        backColor = 'DDD';
                        backColor2 = 'DDD';
                        boxes.forEach((element) => {
                            packIn = false;
                            packOut = false;
                            hasOrder = [];
                            element?.loc.forEach((locElement) => {
                                if (locElement[0] == 'p000000000000000030') packIn = locElement[1]
                                if (locElement[0] == 'p000000000000000060') packOut = locElement[1]
                            })


                            if (packIn || packOut || (() => {
                                temp = false;
                                element?.in.forEach((inElement) => {
                                    if (inElement[0].slice(0, 1) == 'o') {
                                        hasOrder.push(inElement[0]);
                                        packIn = inElement[1];
                                        temp = true;
                                    }
                                })
                                return temp;
                            })()) {
                                if (!hasOrder.length && element.in.length) {
                                    element.in.forEach((inElement) => {
                                        if (inElement[0].slice(0, 1) == 'o') {
                                            hasOrder.push(inElement[0]);
                                        }
                                    })
                                }

                                const matches = [];
                                const missingInCont = [];
                                remainingInCont = [];
                                decl = [];
                                shippingCode = '';
                                orderCode = '';
                                customerName = '';


                                if (hasOrder.length) {
                                    const prefix = "i70000000000"; // Präfix für decl-IDs

                                    // Funktion zum Vergleichen von .decl und .cont
                                    hasOrder.forEach((tempOrder) => {
                                        if (orders.has(tempOrder)) {
                                            // Speichert id und desc, fügt den Präfix zu den IDs hinzu
                                            decl.push(...orders.get(tempOrder).decl.map(([id, desc]) => [`${prefix}${id}`, desc]));
                                        }
                                    });

                                    // cont bleibt unverändert
                                    cont = element.cont.map(([id, timestamp]) => id);

                                    // Zählt die Häufigkeit von IDs in cont
                                    const contCount = cont.reduce((acc, id) => {
                                        acc[id] = (acc[id] || 0) + 1;
                                        return acc;
                                    }, {});

                                    // Prüfen, ob jede decl-ID in cont vorhanden ist
                                    for (const [id, desc] of decl) { // Nutzt id und desc aus decl
                                        if (contCount[id]) {
                                            matches.push(id); // Nur die ID wird für Matches verwendet
                                            contCount[id] -= 1; // Berücksichtigt jede Instanz einzeln
                                            if (contCount[id] === 0) {
                                                delete contCount[id];
                                            }
                                        } else {
                                            missingInCont.push(id); // Nur die fehlende ID wird hinzugefügt
                                        }
                                    }

                                    remainingInCont = Object.keys(contCount); // Nicht verwendete Elemente in cont
                                }




                                // Set zur Vermeidung von Duplikaten
                                const uniqueShippingCodes = new Set();

                                element.brc?.forEach((brcElement, index) => {
                                    const upsRegex = /^1Z[0-9A-Z]{16}$/;
                                    const dpdRegex = /^%[0-9a-zA-Z]{7}(\d{14})\d{6}$/;
                                    const fedExRegex = /^\d{24}$|^\d{34}$/;
                                    const tntRegex = /^\d{4}(\d{9})\d{15}$/;
                                    const dhlExpressRegex = /^JJD\d{9,10}$/;  // Für DHL Express (JJD gefolgt von 9 oder 10 Ziffern)
                                    const dhlPaketRegex = /^\d{12,14}$/;  // Für DHL Paket (12–14-stellige Nummern)
                                    const dhlGlobalMailRegex = /^GM\d{9}DE$/;  // Für DHL Global Mail (GM gefolgt von 9 Ziffern und endet mit DE)
                                    const dhlEcommerceRegex = /^JJ[A-Z0-9]{19}$/; // Für DHL E-Commerce (JJ gefolgt von 20 alphanumerischen Zeichen)

                                    let match;  // Variable für Regex-Matches

                                    if (upsRegex.test(brcElement)) {
                                        uniqueShippingCodes.add(`${brcElement}_UPS`);
                                    } else if (match = brcElement.match(dpdRegex)) {
                                        uniqueShippingCodes.add(`${match[1]}_DPD`);
                                    } else if (match = brcElement.match(tntRegex)) {
                                        uniqueShippingCodes.add(`<a target="_blank" href="https://www.tnt.com/express/en_gb/site/shipping-tools/tracking.html?searchType=con&cons=${match[1]}">${match[1]}_TNT</a>`);
                                    } else if (fedExRegex.test(brcElement)) {
                                        uniqueShippingCodes.add(`${brcElement}_FedEx`);
                                    } else if (dhlExpressRegex.test(brcElement)) {
                                        uniqueShippingCodes.add(`${brcElement}_DHLexp`);
                                    } else if (dhlPaketRegex.test(brcElement) && index === 0) {
                                        uniqueShippingCodes.add(`<a target="_blank" href="https://www.dhl.com/de-en/home/tracking/tracking-parcel.html?submit=1&tracking-id=${brcElement}">${brcElement}_DHLPack</a>`);
                                    } else if (dhlGlobalMailRegex.test(brcElement)) {
                                        uniqueShippingCodes.add(`${brcElement}_DHLmail`);
                                    } else if (dhlEcommerceRegex.test(brcElement)) {
                                        uniqueShippingCodes.add(`<a target="_blank" href="https://www.dhl.com/de-en/home/tracking/tracking-ecommerce.html?submit=1&tracking-id=${brcElement}">${brcElement}_DHLecom</a>`);
                                    }
                                });

                                // Joinen aller eindeutigen Einträge als String
                                shippingCode = Array.from(uniqueShippingCodes).join(' ');




                                const uniqueDev = new Map();

                                element.cont.forEach(dev => {
                                    const serialNumber = dev[0];
                                    const timestamp = dev[1];


                                    if (!uniqueDev.has(serialNumber) || uniqueDev.get(serialNumber).timestamp < timestamp) {
                                        uniqueDev.set(serialNumber, { timestamp, dev });
                                    }
                                });





                                customerName = '';










                                const lastLink = element.in[element.in.length - 1]?.[0];
                                const lastLinkTime = element.in[element.in.length - 1]?.[1];
                                if (
                                    /^o[A-Za-z0-9]{3}RMA\d{10}[A-Za-z0-9]{2}$/.test(lastLink)
                                ) {
                                    if (backColor2 == 'EEE') backColor2 = 'DDD'; else backColor2 = 'EEE';
                                    orderCode = `<a onclick="return myFetch('${lastLink}', 'showOrder', 'outputShow');">${lastLink.slice(2).replace(/^0*/, '')}</a>`
                                    if (orders.has(lastLink)) customerName = orders.get(lastLink).comp;
                                    packIn = lastLinkTime;
                                    htmlContent2 += `<tr style="background-color:#${backColor2}"><td><a onclick="return myFetch('${element.sn[0]}', 'showBox', 'outputShow');">#${(element.sn[0].slice(2).replace(/^0*/, ''))}</a></td>
                                    <td>${packIn ? formatUnixTimestamp(packIn).slice(0, 10) : ''}</td><td>${packIn && packOut ? Math.floor((packOut - packIn) / 60 / 60 / 24) : ''}</td>
                                    <td>${packOut ? formatUnixTimestamp(packOut).slice(0, 10) : ''}</td><td>${customerName}</td><td>${orderCode}</td>
                                    <td><a  onclick="toggleInfoRow(this); return false;">${(() => {
                                            if (uniqueDev.size == matches.length && !missingInCont.length && !remainingInCont.length) {
                                                return '<span style="color:green;">' + uniqueDev.size + '</span>'
                                            } else {
                                                return uniqueDev.size + '/<span style="color:red;">' + remainingInCont.length + '_' + matches.length + '_' + missingInCont.length + '</span>';
                                            }
                                        })()}</a></td><td>${shippingCode}</td></tr>
                                
                                    <tr class="infoRow" style="display:none;">
                                    <td colspan="8" style="text-align:left; background-color:#ee85">${(() => {

                                            let itemsHtml = '';
                                            if (hasOrder) {
                                                uniqueDev.forEach(({ dev }) => {
                                                    const itemTemp = items.get(dev[0]);
                                                    itemsHtml += `<a onclick="return myFetch('${itemTemp.sn[0]}', 'showItem', 'outputShow');">${itemTemp.sn[0].slice(2).replace(/^0*/, '')}</a>: ${decl.find(([entryId, desc]) => entryId === dev[0])?.[1] ?? ''}
                                            <ul>`;

                                                    itemTemp.actn.forEach(([type, message, timestamp]) => {

                                                        itemsHtml += `<li><strong>${type}:</strong> ${message} ( ${formatUnixTimestamp(timestamp)})</li>`;
                                                    });

                                                    itemsHtml += '</ul>';
                                                });
                                            } else {

                                            }
                                            return itemsHtml;

                                        })()}</td></tr>`;
                                } else {
                                    if (element.in.length) {
                                        element.in.forEach((link) => {
                                            if (isBetDirect(link[0]) && link[0].slice(0, 1) == 'o') {
                                                orderCode = link[0].replace(/^o0+/, '');
                                            } else if (isBetDirect(link[0]) && link[0].slice(0, 1) == 'u') {
                                                if (users.has(link[0])) customerName += users.get(link[0]).comp + ' ';
                                            } else {
                                                customerName = link[0] + ' ';
                                            }
                                        })
                                    }
                                    if (backColor == 'EEE') backColor = 'DDD'; else backColor = 'EEE';
                                    htmlContent += `<tr style="background-color:#${backColor}"><td><a onclick="return myFetch('${element.sn[0]}', 'showBox', 'outputShow');">#${(element.sn[0].slice(2).replace(/^0*/, ''))}</a></td>
                                    <td>${packIn ? formatUnixTimestamp(packIn).slice(0, 10) : ''}</td><td>${packIn && packOut ? Math.floor((packOut - packIn) / 60 / 60 / 24) : ''}</td>
                                    <td>${packOut ? formatUnixTimestamp(packOut).slice(0, 10) : ''}</td><td>${customerName}</td><td>${orderCode}</td>
                                    <td><a  onclick="toggleInfoRow(this); return false;">${(() => {
                                            if (uniqueDev.size == matches.length && !missingInCont.length && !remainingInCont.length) {
                                                return '<span style="color:green;">' + uniqueDev.size + '</span>'
                                            } else {
                                                return uniqueDev.size + '/<span style="color:red;">' + remainingInCont.length + '_' + matches.length + '_' + missingInCont.length + '</span>';
                                            }
                                        })()}</a></td><td>${shippingCode}</td></tr>
                                
                                    <tr class="infoRow" style="display:none;">
                                    <td colspan="8" style="text-align:left; background-color:#ee85">${(() => {

                                            let itemsHtml = '';
                                            if (hasOrder) {
                                                uniqueDev.forEach(({ dev }) => {
                                                    const itemTemp = items.get(dev[0]);
                                                    itemsHtml += `<a onclick="return myFetch('${itemTemp.sn[0]}', 'showItem', 'outputShow');">${itemTemp.sn[0].slice(2).replace(/^0*/, '')}</a>: ${decl.find(([entryId, desc]) => entryId === dev[0])?.[1] ?? ''}
                                            <ul>`;

                                                    itemTemp.actn.forEach(([type, message, timestamp]) => {

                                                        itemsHtml += `<li><strong>${type}:</strong> ${message} ( ${formatUnixTimestamp(timestamp)})</li>`;
                                                    });

                                                    itemsHtml += '</ul>';
                                                });
                                            } else {

                                            }
                                            return itemsHtml;

                                        })()}</td></tr>`;
                                }
                            }
                        })
                        htmlContent += `</table><br></span></div><div id='outputTable2' class="cellPaper" style="box-shadow: 0px 10px 20px -10px rgba(255, 255, 255, 0.4)">
                        <span><div style="width: 100%; background-color:#BBB; color: white; font-weight: bold; text-align: center; border: none;">Boxes with Valid RMA Request</div>
                        
                        <table style="width:100%; word-wrap:break-word;border: none;"> 
                        <tr style="background-color:#BBB"><td style="width:80px;">Box</td><td style="width:120px;">inDate</td><td style="width:80px;">Days</td><td style="width:120px;">outDate</td><td>Customer</td><td>Order</td><td>Contain</td><td style="width:200px;">Shipping</td></tr>
                        `;
                        htmlContent += htmlContent2;
                        htmlContent += `</table><br></span></div>
                        <div id='outputTable3' class="cellPaper" style="box-shadow: 0px 10px 20px -10px rgba(255, 255, 255, 0.4)">
                        <span class="text3"><div style="width: 100%; background-color:#BBB; color: white; font-weight: bold; text-align: center; border: none;">Pre-registered RMA requests</div>
                        
                        <table style="width:100%; word-wrap:break-word;"> 
                        <tr style="background-color:#BBB"><td style="width:120px;">Date</td><td style="width:150px;">RMA Order</td><td>Customer</td><td>Person</td><td>Declatation</td></tr>
                        `;
                        orders.forEach((element) => {

                            if (element.cont.length == 0) {
                                declarate = '';
                                element.decl?.forEach((declElement) => {
                                    declarate += `${declElement[0]} `
                                })
                                if (backColor == 'EEE') backColor = 'DDD'; else backColor = 'EEE';
                                htmlContent += `<tr style="background-color:#${backColor}"><td><a onclick="return myFetch('${element.sn[0]}', 'showOrder', 'outputShow');">${formatUnixTimestamp(element.sn[1]).slice(0, 10)}</a></td><td><a onclick="return myFetch('${element.sn[0]}', 'showOrder', 'outputShow');">${element.sn[0].slice(4)}</a></td><td>${element.comp}</td><td>${element.pers}</td><td>${declarate}</td></tr>`
                            }
                        })


                        htmlContentEnd = `</table><br></span></div>
                        <div id='outputShow'></div>
                        `;
                    }
                } else if (Object.hasOwn(payload, 'r')) {
                    const maskedObj = payload.a != 'p' ? maskObjectFields(orders.get('o000' + payload.r), fieldsToMask) : orders.get('o000' + payload.r);
                    htmlContent = '<div style="width: min-content;">' + prettyPrintObject(maskedObj) + '</div>'
                }
            } catch (error) {
                console.error('Fehler:', error.message);
                htmlContent = `${error.message}`;
            }















        }

        if (parsed.name === 'rmaButton') {
            tempRma = unixTime();
            htmlContentStart = `<span class="text3">`
            htmlContent = `
<div style="font-family: Arial, sans-serif; max-width: 600px; margin: 20px auto;">
  <h2>RMA Form</h2>
  <form id="rmaForm" onsubmit="return myFetch('formSubmit', 'rmaForm', 'pdfRma');">
    <input type="text" id="rma" value="RMA${tempRma}${betrugerCrc(tempRma)}" readonly required 
           style="font-size: 20px; width: 95%; padding: 5px; background-color: #eee; margin-top: 5px;">
    <!-- Company Information -->
    <label for="company" style="display: block; margin-top: 10px;"><b>Company Name:</b></label>
    <input type="text" id="company" required 
           style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

    <label for="person" style="display: block; margin-top: 10px;">Contact Person:</label>
    <input type="text" id="person" 
           style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

    <label for="street" style="display: block; margin-top: 10px;"><b>Street and House Number:</b></label>
    <input type="text" id="street" required 
           style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

    <label for="postal" style="display: block; margin-top: 10px;"><b>Postal Code / City:</b></label>
    <input type="text" id="postal" required 
           style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

    <label for="country" style="display: block; margin-top: 10px;"><b>Country:</b></label>
    <input type="text" id="country" required 
           style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

    <!-- Contact Information -->
    <label for="email" style="display: block; margin-top: 10px;"><b>Contact Email:</b></label>
    <input type="email" id="email" required 
           style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

    <label for="invoice_email" style="display: block; margin-top: 10px;">E-Invoice Email:</label>
    <input type="email" id="invoice_email" 
           style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

    <label for="phone" style="display: block; margin-top: 10px;">Phone:</label>
    <input type="tel" id="phone" 
           style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">

    <label for="resellerName" style="display: block; margin-top: 10px;">In case of warranty, please provide the reseller's name:</label>
    <input type="text" id="resellerName" 
           style="width: 95%; padding: 5px; font-size: 20px; background-color: #eee; margin-top: 5px;">


        <!-- Seriennummern und Fehlerbeschreibungen -->
        <br><br><br><br>

        <div style="display: flex; flex-wrap: wrap; margin-top: 10px;">
            <div style="flex: 0 0 170px; margin-right: 10px;">
                <label for="serial1" style=" margin-top: 10px;">Serial Number 1:</label><br>
                <input type="text" id="serial1"   style="width: 170px; padding: 5px; font-size: 20px; background-color : #eee; margin-top: 5px;">
            </div>
            <div style="flex: 1 1 300px;">
                <label for="description1" style=" margin-top: 10px;">Issue Description:</label><br>
                <textarea id="description1"  rows="3"  style="width: 95%; min-width: 300px; padding: 5px; font-size: 20px; background-color : #eee; margin-top: 5px;"></textarea>
            </div>
        </div>
        <br>
        <div style="display: flex; flex-wrap: wrap; margin-top: 10px;">
            <div style="flex: 0 0 170px; margin-right: 10px;">
                <label for="serial2" style=" margin-top: 10px;">Serial Number 2:</label><br>
                <input type="text" id="serial2"  style="width: 170px; padding: 5px; font-size: 20px; background-color : #eee; margin-top: 5px;">
            </div>
            <div style="flex: 1 1 300px;">
                <label for="description2" style=" margin-top: 10px;">Issue Description:</label><br>
                <textarea id="description2"  rows="3"  style="width: 95%; min-width: 300px; padding: 5px; font-size: 20px; background-color : #eee; margin-top: 5px;"></textarea>
            </div>
        </div>
        <br>
                <div style="display: flex; flex-wrap: wrap; margin-top: 10px;">
            <div style="flex: 0 0 170px; margin-right: 10px;">
                <label for="serial3" style=" margin-top: 10px;">Serial Number 3:</label><br>
                <input type="text" id="serial3"   style="width: 170px; padding: 5px; font-size: 20px; background-color : #eee; margin-top: 5px;">
            </div>
            <div style="flex: 1 1 300px;">
                <label for="description3" style=" margin-top: 10px;">Issue Description:</label><br>
                <textarea id="description3"  rows="3"  style="width: 95%; min-width: 300px; padding: 5px; font-size: 20px; background-color : #eee; margin-top: 5px;"></textarea>
            </div>
        </div>
        <br>
        <div style="display: flex; flex-wrap: wrap; margin-top: 10px;">
            <div style="flex: 0 0 170px; margin-right: 10px;">
                <label for="serial4" style=" margin-top: 10px;">Serial Number 4:</label><br>
                <input type="text" id="serial4"   style="width: 170px; padding: 5px; font-size: 20px; background-color : #eee; margin-top: 5px;">
            </div>
            <div style="flex: 1 1 300px;">
                <label for="description4" style=" margin-top: 10px;">Issue Description:</label><br>
                <textarea id="description4"  rows="3"  style="width: 95%; min-width: 300px; padding: 5px; font-size: 20px; background-color : #eee; margin-top: 5px;"></textarea>
            </div>
        </div>
        <br>
                <div style="display: flex; flex-wrap: wrap; margin-top: 10px;">
            <div style="flex: 0 0 170px; margin-right: 10px;">
                <label for="serial5" style=" margin-top: 10px;">Serial Number 5:</label><br>
                <input type="text" id="serial5"   style="width: 170px; padding: 5px; font-size: 20px; background-color : #eee; margin-top: 5px;">
            </div>
            <div style="flex: 1 1 300px;">
                <label for="description5" style=" margin-top: 10px;">Issue Description:</label><br>
                <textarea id="description5"  rows="3"  style="width: 95%; min-width: 300px; padding: 5px; font-size: 20px; background-color : #eee; margin-top: 5px;"></textarea>
            </div>
        </div>
        <br>
    <!-- Submit Button -->
    <button class="buttonFlat" type="submit" style="font-size: 20px;margin: 5px; margin-left: max(calc((100% - 450px)/10),0px);float:left;"> 
      Submit Form
    </button> 
  </form>

      <button class="buttonFlat" type="" onclick="location.reload()" style="font-size: 20px; margin: 5px; margin-right: max(calc((100% - 450px)/10),0px);float:right;"> 
      Back
    </button> 
    <br><br><br>
</div>
            `;
            htmlContentEnd = `<br></span>`;
        } else if (parsed.name === 'snInput') {
            if (/^showmealldevices(\d{8})$/i.test(parsed.text)) {

                htmlContentStart = ``;
                htmlContent = '';

                // Extract the date part from the command
                const matches = parsed.text.match(/^showmealldevices(\d{8})$/i);
                if (!matches) {
                    throw new Error("Command does not match the expected format.");
                }
                const dateStr = matches[1]; // YYYYMMDD

                // Parse the date string into components
                const year = parseInt(dateStr.substring(0, 4), 10);
                const month = parseInt(dateStr.substring(4, 6), 10) - 1; // Months are 0-indexed in JavaScript Date
                const day = parseInt(dateStr.substring(6, 8), 10);

                // Validate the parsed date
                if (isNaN(year) || isNaN(month) || isNaN(day)) {
                    throw new Error("Invalid date format.");
                }

                // Create a Date object and convert it to Unix timestamp (seconds)
                const date = new Date(year, month, day, 0, 0, 0);
                const timestamp = Math.floor(date.getTime() / 1000);



                // Initialize an array to hold matching device keys
                let matchingDeviceKeys = [];

                // Iterate through all items to find 'i7' devices created after the specified date
                items.forEach((device, key) => {
                    if (
                        key.startsWith('i7') &&
                        device.sn &&
                        device.sn.length > 1 &&
                        device.sn[1] > timestamp
                    ) {
                        matchingDeviceKeys.push(key);
                    }
                });

                // Build HTML content based on matching devices
                if (matchingDeviceKeys.length > 0) {
                    htmlContent += `<b>Devices created after ${formatUnixTimestamp(timestamp)}:</b><br>`;
                    htmlContent += `
                                <table border="1" cellspacing="0" cellpadding="5">
                                    <thead>
                                        <tr>
                                            <th>Device SN</th>
                                            <th>Registered On</th>
                                            <th>Returned On</th>
                                        </tr>
                                    </thead>
                                    <tbody>
                            `;

                    matchingDeviceKeys.forEach(deviceKey => {
                        const device = items.get(deviceKey);
                        let returned = false;
                        let returnedTimestamp = null;

                        // Check the last location of the device
                        if (device.loc && device.loc.length > 0) {
                            const lastDeviceLoc = device.loc[device.loc.length - 1];
                            const lastLocCode = lastDeviceLoc[0];
                            const lastLocTime = lastDeviceLoc[1];

                            if (lastLocCode === 'p000000000000000060') {
                                returned = true;
                                returnedTimestamp = lastLocTime;
                            } else if (lastLocCode.startsWith('b')) {
                                // If last location starts with 'b', it's a box. Check the box's last location.
                                const box = boxes.get(lastLocCode);
                                if (box && box.loc && box.loc.length > 0) {
                                    const lastBoxLoc = box.loc[box.loc.length - 1];
                                    if (lastBoxLoc[0] === 'p000000000000000060') {
                                        returned = true;
                                        returnedTimestamp = lastBoxLoc[1];
                                    }
                                }
                            }
                        }

                        // Calculate days since registration
                        const currentTime = Math.floor(Date.now() / 1000); // Current Unix timestamp in seconds
                        const daysSinceRegistration = Math.floor((currentTime - device.sn[1]) / 86400); // 86400 seconds in a day

                        // Determine font color
                        const fontColor = getFontColor(daysSinceRegistration, returned);

                        // Format dates
                        const registeredOn = formatUnixTimestamp(device.sn[1]);
                        const returnedOn = returned
                            ? formatUnixTimestamp(returnedTimestamp)
                            : 'Not Returned';

                        // Add a table row with the determined font color
                        htmlContent += `
                                    <tr>
                                        <td style="${fontColor}"><strong>${device.sn[0].slice(-7)}</strong></td>
                                        <td style="${fontColor}">${registeredOn}</td>
                                        <td style="${fontColor}">${returnedOn}</td>
                                    </tr>
                                `;
                    });

                    htmlContent += `
                                    </tbody>
                                </table>
                            `;
                } else {
                    htmlContent += `<em>No 'i7' devices found created after ${formatUnixTimestamp(timestamp)}.</em>`;
                }

                htmlContentEnd = ``;

            }
            // Validate that the serial number consists of exactly 7 digits
            else if (/^\d{7}$/.test(parsed.text)) {
                htmlContentStart = ``;
                htmlContent = '';

                // Construct the device key based on the input serial number
                const deviceKey = 'i70000000000' + parsed.text;

                // Check if the device exists in the items map
                if (items.has(deviceKey)) {
                    const device = items.get(deviceKey);

                    // Display Device Serial Number and Registration Time
                    htmlContent += `Device SN: <strong>${parsed.text}</strong><br>`;
                    htmlContent += `Registered on: ${formatUnixTimestamp(device.sn[1])}<br><br>`;

                    // Display Actions (excluding 'note') with formatted dates
                    if (device.actn && device.actn.length > 0) {
                        htmlContent += `Actions:<ul>`;
                        device.actn.forEach(action => {
                            const actionType = action[0];
                            const actionText = action[1];
                            const actionTimestamp = action[2];

                            // Exclude actions of type 'note'
                            if (actionType.toLowerCase() !== 'note') {
                                let style = "";

                                // Conditional styling for 'result' actions based on timestamp
                                if (actionType.toLowerCase() === 'result') {
                                    if (actionTimestamp > device.sn[1]) {
                                        // 'Result' action after registration time - Green
                                        style = 'style="color: green;"';
                                    } else {
                                        // 'Result' action before registration time - Red
                                        style = 'style="color: red;"';
                                    }
                                }

                                // Capitalize the first letter of the action type
                                const capitalizedType = actionType.charAt(0).toUpperCase() + actionType.slice(1);

                                // Format the action timestamp
                                const actionDate = formatUnixTimestamp(actionTimestamp);

                                // Append the action to the HTML content with appropriate styling
                                if (style) {
                                    htmlContent += `<li ${style}>${capitalizedType}: ${actionText} (${actionDate})</li>`;
                                } else {
                                    htmlContent += `<li>${capitalizedType}: ${actionText} (${actionDate})</li>`;
                                }
                            }
                        });
                        htmlContent += `</ul><br><br>`;
                    } else {
                        htmlContent += `<em>No actions available.</em><br><br>`;
                    }

                    // Determine the device's current location
                    let currentLocID = '';
                    if (device.loc && device.loc.length > 0) {
                        const lastLoc = device.loc[device.loc.length - 1];
                        currentLocID = lastLoc[0];
                        const currentLocTimestamp = lastLoc[1];
                        htmlContent += `Current Location: ${currentLocID} (${formatUnixTimestamp(currentLocTimestamp)})<br>`;
                    } else {
                        htmlContent += `<em>No location information available.</em><br>`;
                    }

                    // Find the box containing the device to check its last location
                    let boxCompleted = false;
                    boxes.forEach(box => {
                        if (box.cont && box.cont.some(cont => cont[0] === deviceKey)) {
                            if (box.loc && box.loc.length > 0) {
                                const lastBoxLoc = box.loc[box.loc.length - 1][0];
                                if (lastBoxLoc === 'p000000000000000060') {
                                    boxCompleted = true;
                                }
                            }
                        }
                    });

                    // Check if either the device's last location or its box's last location is 'p000000000000000060'
                    if (currentLocID === 'p000000000000000060' || boxCompleted) {
                        htmlContent += `<br><em style="color: green; font-weight: bold;">Device has been sent back to the client by mail.</em>`;
                    }
                } else {
                    // Device not found in the items map
                    htmlContent += `<br><em>Device not found.</em>`;
                }

                htmlContentEnd = ``;
            } else if (/^RMA\d{10}[A-Za-z0-9]{2}$/.test(parsed.text)) {
                htmlContentStart = ``;
                htmlContent = '';
                const orderKey = 'o000' + parsed.text;

                if (orders.has(orderKey)) {
                    const show = orders.get(orderKey);
                    htmlContent += `The RMA contract <b>${parsed.text}</b> was submitted on<br>`;

                    // **Replaced prettyPrintObject with formatUnixTimestamp**
                    htmlContent += `${formatUnixTimestamp(show.sn[1])}<br>`;

                    // **Added two <br> tags before "Received in Package(s):"**
                    htmlContent += `<br><br>`;

                    // Check for declared issues
                    if (show.decl && show.decl.length > 0) {
                        htmlContent += `<div class="declared-issues"><b>Declared Issues:</b><ul>`;
                        show.decl.forEach(declaration => {
                            const issueID = declaration[0];
                            const issueDescription = declaration[1];
                            htmlContent += `<li><strong>SN:</strong> ${issueID} - ${issueDescription}</li>`;
                        });
                        htmlContent += `</ul></div>`;
                    } else {
                        htmlContent += `<div class="no-issues"><em>No declared issues.</em></div>`;
                    }

                    // **Added couple of <br> tags after Declared Issues to separate sections**
                    htmlContent += `<br><br>`;

                    // Initialize a flag to track if contract completion message should be added
                    let contractCompleted = false;

                    // Check for boxes in 'cont'
                    if (show.cont && show.cont.length > 0) {
                        htmlContent += `<div class="received-packages"><b>Received in Package(s):</b><ul>`;
                        show.cont.forEach(container => {
                            const boxKey = container[0]; // e.g., "b000000000000000109"
                            const registrationTime = container[1]; // Unix timestamp

                            if (boxes.has(boxKey)) {
                                const box = boxes.get(boxKey);
                                // **Replaced prettyPrintObject with formatUnixTimestamp**
                                const formattedTime = formatUnixTimestamp(registrationTime);

                                // Inline Formatting: Remove 'b' and leading zeros from box serial number
                                const formattedBoxSN = box.sn[0].replace(/^b0+/, '');

                                htmlContent += `<li><strong>Box Number:</strong> ${formattedBoxSN}<br>`;
                                htmlContent += `<strong>Registration Time:</strong> ${formattedTime}<br>`;

                                // **Check if the box has loc 'p000000000000000060' and its timestamp > RMA submit time**
                                if (box.loc && Array.isArray(box.loc)) {
                                    // Find the loc entry for 'p000000000000000060'
                                    const locEntry = box.loc.find(loc => loc[0] === 'p000000000000000060');

                                    if (locEntry && locEntry[1] > show.sn[1]) {
                                        contractCompleted = true;
                                    }
                                }

                                // List devices in the box
                                if (box.cont && box.cont.length > 0) {
                                    htmlContent += `<strong>Registered Devices:</strong><ul>`;
                                    box.cont.forEach(device => {
                                        let deviceSN = device[0]; // e.g., "i700000000002113897"
                                        const deviceTime = device[1];
                                        // **Replaced prettyPrintObject with formatUnixTimestamp**
                                        const deviceFormattedTime = formatUnixTimestamp(deviceTime);

                                        // Inline Formatting: Extract last 7 characters if deviceSN starts with 'i7'
                                        if (deviceSN.startsWith('i7') && deviceSN.length >= 7) {
                                            deviceSN = deviceSN.slice(-7);
                                        }

                                        // **Made Device SN bold by wrapping it in <strong> tags**
                                        htmlContent += `<li>Device SN: <strong>${deviceSN}</strong><br>Registered on: ${deviceFormattedTime}`;

                                        // Retrieve and display action details from items map
                                        if (items.has(device[0])) { // Using original device SN for lookup
                                            const item = items.get(device[0]);
                                            if (item.actn && item.actn.length > 0) {
                                                // **Made "Actions" regular (not bold) by removing <strong> tags**
                                                htmlContent += `<br>Actions:<ul>`;
                                                item.actn.forEach(action => {
                                                    const actionType = action[0];
                                                    const actionText = action[1];
                                                    const actionTimestamp = action[2]; // Unix timestamp of the action

                                                    // Exclude 'note' actions
                                                    if (actionType.toLowerCase() !== 'note') {
                                                        let style = "";

                                                        // **Conditional styling for 'result' actions**
                                                        if (actionType.toLowerCase() === 'result') {
                                                            if (actionTimestamp > show.sn[1]) {
                                                                // Result action after RMA submission time - Green
                                                                style = 'style="color: green;"';
                                                            } else {
                                                                // Result action before RMA submission time - Red
                                                                style = 'style="color: red;"';
                                                            }
                                                        }

                                                        // Capitalize the first letter of the action type
                                                        const capitalizedType = actionType.charAt(0).toUpperCase() + actionType.slice(1);

                                                        // **Formatted date for each Action**
                                                        const actionDate = formatUnixTimestamp(actionTimestamp);

                                                        // Apply the style if applicable and include the date
                                                        if (style) {
                                                            htmlContent += `<li ${style}>${capitalizedType}: ${actionText} (${actionDate})</li>`;
                                                        } else {
                                                            htmlContent += `<li>${capitalizedType}: ${actionText} (${actionDate})</li>`;
                                                        }
                                                    }
                                                });
                                                // **Added two <br> tags after Actions**
                                                htmlContent += `</ul><br><br>`;
                                            }
                                        }

                                        htmlContent += `</li>`;
                                    });
                                    htmlContent += `</ul>`;
                                } else {
                                    htmlContent += `<em>No devices registered in this box.</em>`;
                                }

                                htmlContent += `</li>`;
                            } else {
                                htmlContent += `<li><strong>Box Key:</strong> ${boxKey} - <em>Box details not found.</em></li>`;
                            }
                        });
                        htmlContent += `</ul></div>`;
                    } else {
                        htmlContent += `<div class="no-packages"><em>No packages associated with this order.</em></div>`;
                    }

                    // **Append the completion message if the condition is met, making it bold and green**
                    if (contractCompleted) {
                        htmlContent += `<br><br><em style="color: green; font-weight: bold;">Contract completed and sent back to the client by mail. For detailed information, use the login you received when creating the RMA form.</em>`;
                    }
                } else {
                    htmlContent += `<br><em>Order not found.</em>`;
                }
                htmlContentEnd = ``;
            } else if (!parsed.jwt) {
                try {
                    const payload = verifyJWT(parsed.text.replace(/\s+/g, ''), secretJwt);
                    console.log('JWT gültig:', payload);
                    if (Object.hasOwn(payload, 'u')) {
                        htmlContent = parsed.text.replace(/\s+/g, '');

                    } else if (Object.hasOwn(payload, 'r')) {
                        htmlContent = parsed.text.replace(/\s+/g, '');
                    }
                } catch (error) {
                    console.error('JWT Fehler:', error.message);
                    htmlContent = ``;
                }
            }
        } else if (parsed.text.slice(0, 5) === 'parts') {
            bodyReq = parsed.text.slice(5);
            //console.log(bodyReq)
            htmlContentStart = `<span class="text3">
            <table style="width:100%;word-wrap: break-word;"> 
            <tr>
            <td colspan="3" class="cellPaper">Teile für ${bodyReq}</td>               
            <td colspan="4" class="inputPaper"><span class="text2blue">Die gewünschte<wbr> Bestellung</span></td>
            </tr>`
            htmlContent = '';

            upperTemp = uppers.get(bodyReq);
            if (upperTemp && Object.hasOwn(upperTemp, 'down')) {
                upperTemp.down.map((va) => {

                    if (va[0] === 'c') {
                        classTemp = classes.get(va[1]);

                        if (classTemp && Object.hasOwn(classTemp, 'down')) {
                            classTemp.down.map((valu) => {

                                if (valu[0] === 'i') {
                                    itemTemp = items.get(valu[1]);
                                    if (itemTemp) {
                                        htmlContent += `             
        <tr  >
        <td class="cellPaper" style="width:80px;">${(itemTemp.cl.toString().match(/.{1,4}/g) || []).map(el => el).join('<wbr>')}</td>
        <td class="imgPaper" style="width:160px;"><img loading="lazy" src="${itemTemp.img[0]}"  onclick="fsPic('${itemTemp.img[0]}')" width="160px"></td>
        <td  class="cellPaper" style="width:80px;"><b>${itemTemp.mult.map(el => el[0]).join('<br>')}</b></td>
        <td  class="inputPaper" style="width:80px;"><span class="text2blue"><div id="${itemTemp.cl}" onfocus="onFocusInt('${itemTemp.cl}')" onfocusout="onFocusOutInt('${itemTemp.cl}',${itemTemp.mult.map(el => el[0]).join('<br>')})" contenteditable="true">0</div></span></td>
        <td  class="cellPaper" style="width:80px;"><small>${itemTemp.rel.map(el => el[0] == 'partOf' ? `${el[1]}` : '').join('<br>')}</small></td>
        <td  class="cellPaper" style="width:80px;"><small><b>${betrugerCrc(itemTemp.sn[0].slice(2).replace(/^0*/, ''))}</b><br>${fullPath(itemTemp).join('<br>')}</small></td>
        <td  class="cellPaper"><small><small>${itemTemp.desc.join('<br>')}</small></small></td>
        </tr>`;
                                    }
                                }
                            })
                        }

                    }
                });
            }

            htmlContentEnd = `</table><br></span>`;
        }
        if (parsed.name === 'rmaForm') {
            rmaJs = JSON.parse(parsed.text);
            htmlContentStart = `<span class="text3">`
            htmlContent = ``;
            htmlContentEnd = `<br></span>`
        } return htmlContentStart + htmlContent + htmlContentEnd;
    } catch (error) {
        return `
    <html>
        <head><title>Antwort</title></head>
        <body>
            <h1>Empfangene Daten</h1>
            <p>${bodyReq}</p><br>
            <h1>Fehler</h1>
            <p>${error}</p>
        </body>
    </html>`;
    }
};














function generatePdfRma(rmaJs, link, token, code) {
    return new Promise((resolve, reject) => {
        const fonts = {
            Roboto: {
                normal: 'fonts/Roboto-Regular.ttf',
                bold: 'fonts/Roboto-Medium.ttf',
                italics: 'fonts/Roboto-Italic.ttf',
                bolditalics: 'fonts/Roboto-MediumItalic.ttf',
            },
        };

        try {
            const dd = {
                content: [
                    // Erste Seite
                    {
                        text: `${rmaJs.rma}`,
                        style: 'header',
                    },
                    {
                        columns: [
                            {
                                text: [
                                    rmaJs.company ? { text: `${rmaJs.company}\n`, bold: true } : '',
                                    rmaJs.person ? `${rmaJs.person}\n` : '',
                                    rmaJs.street ? `${rmaJs.street}\n` : '',
                                    rmaJs.postal ? `${rmaJs.postal}\n` : '',
                                    rmaJs.country ? `${rmaJs.country}` : '',
                                ].filter(Boolean),
                                width: '35%',
                            },
                            {
                                qr: link, // QR Code mit dem Tracking-Link
                                fit: '110',
                                foreground: '#000088',
                                width: '30%',
                            },
                            {
                                text: [
                                    { text: 'M3 Mobile GmbH\n', bold: true },
                                    'Am Holzweg 26\n',
                                    '65830 Kriftel\n',
                                    'Deutschland',
                                ],
                                alignment: 'right',
                                width: '35%',
                            },
                        ],
                        columnGap: 10,
                        margin: [0, 20, 0, 20],
                    },
                    {
                        columns: [
                            {
                                text: [
                                    rmaJs.email
                                        ? { text: `Contact Email: ${rmaJs.email}\n`, bold: true }
                                        : '',
                                    rmaJs.invoice_email
                                        ? { text: `E-Invoice Email: ${rmaJs.invoice_email}\n`, bold: true }
                                        : '',
                                    rmaJs.phone ? { text: `Phone: ${rmaJs.phone}\n`, bold: true } : '',
                                    rmaJs.resellerName
                                        ? { text: `Reseller: ${rmaJs.resellerName}\n`, bold: true }
                                        : '',
                                ].filter(Boolean),
                                width: '70%',
                            },
                            {
                                qr: token, // QR Code mit dem JWT
                                fit: '100',
                                foreground: '#006600',
                                alignment: 'right',
                                width: '30%',
                            },
                        ],
                        columnGap: 10,
                        margin: [0, 20, 0, 20],
                    },
                    {
                        text: 'RMA Tracking Link:',
                        margin: [0, 10, 0, 5],
                    },
                    {
                        text: link,
                        link: link, // Zum Anklicken
                        color: '#000088',
                        margin: [0, 0, 0, 10],
                    },
                    {
                        text: 'RMA Access Token for full access:',
                        margin: [0, 10, 0, 5],
                    },
                    {
                        text: token,
                        color: '#006600',
                        margin: [0, 0, 0, 10],
                    },
                    {
                        text: 'Serial Numbers and Issue Descriptions',
                        style: 'subheader',
                        margin: [0, 20, 0, 10],
                    },
                    // Dynamisch generierte Seriennummern und Beschreibungen
                    ...Object.keys(rmaJs)
                        .filter((key) => key.startsWith('serial') && rmaJs[key]) // Nur Serial-Felder verwenden
                        .map((serialKey, index) => {
                            const serial = rmaJs[serialKey];
                            const descriptionKey = `description${serialKey.replace('serial', '')}`;
                            const description = rmaJs[descriptionKey];

                            return serial && description
                                ? {
                                    columns: [
                                        { text: `${serial}`, width: 100 },
                                        { text: `${description}`, width: '*' },
                                    ],
                                    margin: [0, 5, 0, 5],
                                }
                                : [];
                        }),
                    // Zweite Seite
                    {
                        text: '',
                        pageBreak: 'before', // Neue Seite
                    },
                    {
                        text: [
                            { text: 'M3 Mobile GmbH\n', bold: true },
                            'Am Holzweg 26\n',
                            '65830 Kriftel\n',
                            'Deutschland\n',
                        ],
                        absolutePosition: { x: 70, y: 150 }, // Position im Brieffenster
                        fontSize: 12,
                    },
                    {
                        qr: `ECK1.COM/${code}M3`,
                        fit: '58',
                        absolutePosition: { x: 180, y: 150 }, // Position im Brieffenster
                    },
                    {
                        columns: [
                            {
                                text: `Please send us only this sheet or write ${rmaJs.rma} on the parcel.`,
                                style: 'subheader',
                                color: '#880000',
                                width: '70%',
                            },
                            {
                                qr: `ECK2.COM/${code}M3`,
                                fit: '58',
                                alignment: 'right',
                                width: '15%',
                            },
                            {
                                qr: `ECK3.COM/${code}M3`,
                                fit: '58',
                                alignment: 'right',
                                width: '15%',
                            },
                        ],
                        columnGap: 10,
                        margin: [0, 18, 0, 18],
                    },
                    {
                        columns: [
                            {
                                qr: `ECK1.COM/${code}M3`,
                                fit: '58',
                                alignment: 'right',
                                width: '70%',
                            },
                            {
                                qr: `ECK2.COM/${code}M3`,
                                fit: '58',
                                alignment: 'right',
                                width: '15%',
                            },
                            {
                                qr: `ECK3.COM/${code}M3`,
                                fit: '58',
                                alignment: 'right',
                                width: '15%',
                            },
                        ],
                        columnGap: 10,
                        margin: [0, 18, 0, 18],
                    },
                    {
                        canvas: [
                            // Erste punktierte Linie (1/3 von A4)
                            {
                                type: 'line',
                                x1: 15,
                                y1: 50,
                                x2: 495,
                                y2: 50,
                                lineWidth: 1,
                                dash: { length: 5 }, // Punktierte Linie
                            },
                        ],
                    },
                    {
                        text: 'Serial Numbers and Issue Descriptions',
                        style: 'subheader',
                        margin: [0, 30, 0, 10],
                    },
                    // Dynamisch generierte Seriennummern und Beschreibungen
                    ...Object.keys(rmaJs)
                        .filter((key) => key.startsWith('serial') && rmaJs[key]) // Nur Serial-Felder verwenden
                        .map((serialKey, index) => {
                            const serial = rmaJs[serialKey];
                            const descriptionKey = `description${serialKey.replace('serial', '')}`;
                            const description = rmaJs[descriptionKey];

                            return serial && description
                                ? {
                                    columns: [
                                        { text: `${serial}`, width: 100 },
                                        { text: `${description}`, width: '*' },
                                    ],
                                    margin: [0, 5, 0, 5],
                                }
                                : [];
                        }),
                ],
                styles: {
                    header: {
                        fontSize: 22,
                        bold: true,
                        margin: [0, 0, 0, 10],
                    },
                    subheader: {
                        fontSize: 18,
                        bold: true,
                        margin: [0, 10, 0, 5],
                    },
                },
            };

            const printer = new PdfPrinter(fonts);
            const pdfDoc = printer.createPdfKitDocument(dd);

            let chunks = [];
            pdfDoc.on('data', (chunk) => {
                chunks.push(chunk);
            });

            pdfDoc.on('end', () => {
                resolve(Buffer.concat(chunks));
            });

            pdfDoc.end();
        } catch (err) {
            reject(err);
        }
    });
}






// PDF-Inhalt
const pdfDefinition = {
    content: [
        'Dies ist ein Beispielinhalt für die PDF-Datei.',
        'Sie wird im Speicher erstellt und direkt an den Client gesendet.'
    ]
};












const getPostData = (req) => {
    return new Promise((resolve, reject) => {
        let body = '';
        req.on('data', chunk => {
            body += chunk.toString();
        });
        req.on('end', () => {
            resolve(body);
        });
        req.on('error', (err) => {
            reject(err);
        });
    });
};

function splitStreetAndHouseNumber(address) {
    try {
        if (typeof address !== "string") {
            throw new Error("Address must be a string");
        }

        // Aktualisierter RegExp zur Unterstützung verschiedener Adressformate
        const regex = /^(.*?)(\d{1,5}[A-Za-z\-\/\s]*)$/;

        const match = address.trim().match(regex);

        if (match) {
            const street = match[1].trim(); // Straße
            const houseNumber = match[2].trim(); // Hausnummer

            return { street, houseNumber };
        } else {
            // Kein Match: Ganze Adresse als Straße zurückgeben
            return { street: address.trim(), houseNumber: "" };
        }
    } catch (error) {
        console.error("Error while splitting address:", error.message);
        // Fallback: Alles als Straße speichern
        return { street: address.trim(), houseNumber: "" };
    }
}


function splitPostalCodeAndCity(address) {
    const regex = /(\d{4,6})\s*([A-Za-z]*)/;
    const match = address.trim().match(regex);

    if (match) {
        const postalCode = match[1];  // Die Postleitzahl
        const city = match[2] || address.replace(postalCode, '').trim();  // Der Rest wird als Stadt behandelt
        return { city, postalCode };
    } else {
        // Wenn kein gültiger Match gefunden wird, behandeln wir die gesamte Eingabe als Stadt
        return { city: address.trim(), postalCode: '' };
    }
}


function convertToSerialDescriptionArray(rmaJs) {
    const result = [];

    // Durchlaufe die Keys des rmaJs-Objekts
    for (let key in rmaJs) {
        // Prüfen, ob der Key mit "serial" beginnt
        if (key.startsWith('serial')) {
            const index = key.replace('serial', '');  // Extrahiere die Nummer von serial1, serial2, etc.
            const serial = rmaJs[key];  // Seriennummer
            const descriptionKey = `description${index}`;  // Entsprechender Description Key
            const description = rmaJs[descriptionKey];  // Beschreibung

            if (serial && description) {
                result.push([serial, description]);  // Füge das Paar in das Ergebnis-Array hinzu
            }
        }
    }

    return result;
}


const payload3 = {
    u: 'Brian',
    a: 'p',
    e: unixTime() + 60 * 60 * 24 * 90 // Ablauf in einem Monat
};



// JWT generieren
console.log(generateJWT(payload3, secretJwt))




const requestHandler = async (req, res) => {
    try {
        if (req.url === '/view-for-mr-cho' && req.method === 'GET') {
            res.statusCode = 200;
            res.setHeader('Content-Type', 'text/html');
            res.end('<!DOCTYPE html><html><head><script>localStorage.setItem("jwt", "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJ1IjoiQW50aG9ueSIsImEiOiJwIiwiZSI6MTc0MDYxODA4OX0.Is8HjJ9oumU_Ol_kYeFFog4il7hwLhRKdPLYSl19Wp0"); window.location.href = "/";</script></head><body></body></html>');
        } else if (req.url === '/' && req.method === 'GET') {
            const filePath = join(__dirname, '/html/', 'index.html');
            const data = await readFile(filePath);
            res.statusCode = 200;
            res.setHeader('Content-Type', 'text/html');
            res.end(data);
        } else if (req.url.startsWith('/storage/') && req.method === 'GET') {
            const filePath = join(__dirname, '/html/', req.url);
            const data = await readFile(filePath);
            res.statusCode = 200;
            res.setHeader('Content-Type', 'image/png');
            res.end(data);
        } else if (req.url.startsWith('/jwt/') && req.method === 'GET') {
            res.statusCode = 200;
            res.setHeader('Content-Type', 'text/html');
            res.end((() => {
                try {
                    const payload = verifyJWT(req.url.slice(5), secretJwt);
                    const maskedObj = payload.a != 'p' ? maskObjectFields(orders.get('o000' + payload.r), fieldsToMask) : orders.get('o000' + payload.r);
                    return '<div style="width: min-content;">' + prettyPrintObject(maskedObj) + '</div><br><br><a href="https://m3.repair/" style="color:#1e2071;">M3 Mobile GmbH Homepage</a>';
                } catch (error) {
                    return ' nice try';
                }
            })());
        } else if (req.url === '/' && req.method === 'POST' && req.headers['content-type'] === 'application/json') {
            const bodyRequest = await getPostData(req);
            console.log('Empfangene Daten:', bodyRequest);

            const parsedData = JSON.parse(bodyRequest);

            if (parsedData.dest === 'pdfRma') {

                rmaJson = JSON.parse(parsedData.text);

                // Beispiel Payload und Secret
                const payload1 = {
                    r: rmaJson.rma.trim(),
                    a: 'l',
                    e: unixTime() + 60 * 60 * 24 * 30 // Ablauf in einem Monat
                };
                const payload2 = {
                    r: rmaJson.rma.trim(),
                    a: 'p',
                    e: unixTime() + 60 * 60 * 24 * 90 // Ablauf in einem Monat
                };


                // JWT generieren
                const token1 = generateJWT(payload1, secretJwt);
                const token2 = generateJWT(payload2, secretJwt);
                const linkToken = `https://m3.repair/jwt/${token1}`;
                //console.log(token1, token2)


                /*
                                try {
                                    const payload = verifyJWT(token, secret);
                                    console.log('JWT gültig:', payload);
                                } catch (error) {
                                    console.error('JWT-Verifikationsfehler:', error.message);
                                }
                */

                formattedInput = rmaJson.rma.trim(); // Entfernt etwaige Leerzeichen am Anfang und Ende
                if (formattedInput.length > 18) {
                    throw new Error("Der Eingabewert ist zu lang.");
                }
                // Füge 'o' am Anfang hinzu und fülle den Rest mit '0', bis die Länge 19 erreicht ist
                formattedInput = 'o' + formattedInput.padStart(18, '0');
                const pdfBuffer = await generatePdfRma(rmaJson, linkToken, token2, betrugerUrlEncrypt(formattedInput));
                res.writeHead(200, {
                    'Content-Type': 'application/pdf',
                    'Content-Disposition': 'attachment; filename="example.pdf"',
                    'Content-Length': pdfBuffer.length
                });
                res.end(pdfBuffer);

                res.on('finish', async () => {
                    console.log('create ' + formattedInput)
                    const tempObj = Object.create(order);
                    tempObj.sn = [formattedInput, unixTime()];
                    tempObj.cust = { 'reseller': rmaJson.resellerName };
                    tempObj.comp = rmaJson.company;
                    tempObj.pers = rmaJson.person;
                    const addressInfo1 = splitStreetAndHouseNumber(rmaJson.street);
                    tempObj.str = addressInfo1.street;
                    tempObj.hs = addressInfo1.houseNumber;
                    const addressInfo2 = splitPostalCodeAndCity(rmaJson.postal);
                    tempObj.zip = addressInfo2.postalCode;
                    tempObj.cit = addressInfo2.city;

                    tempObj.ctry = rmaJson.country;
                    tempObj.cem = rmaJson.email;
                    tempObj.iem = rmaJson.invoice_email;
                    tempObj.ph = rmaJson.phone;
                    tempObj.cont = [];
                    tempObj.decl = convertToSerialDescriptionArray(rmaJson);;
                    orders.set(formattedInput, tempObj);
                    try {
                        await writeLargeMapToFile(orders, resolve(`${baseDirectory}base/orders.json`))
                    } catch (err) {
                        // When a request is aborted - err is an AbortError
                        console.error(err);
                    }
                });


            } else if (parsedData.dest === 'csv') {
                csv = 'SN /PN;Model;IN DATE;Out Date;Customer;SKU;email;Address;Zip Code;City;Complaint;Verification;Cause;Result;Shipping;Invoice number;Special note; warranty;condition;Used New Parts;Used Refurbished Parts\n';

                boxes.forEach((element) => {
                    packIn = false;
                    packOut = false;
                    element.loc?.forEach((locElement) => {
                        if (locElement[0] == 'p000000000000000030') packIn = locElement[1]
                        if (locElement[0] == 'p000000000000000060') packOut = locElement[1]
                    })

                    if (packIn || packOut) {
                        shippingCode = '';
                        orderCode = '';
                        customerName = '';
                        Bemail = '';
                        BAddress = '';
                        BZipCode = '';
                        BCity = '';
                        element.brc?.forEach((brcElement, index) => {
                            const upsRegex = /^1Z[0-9A-Z]{16}$/;
                            const dpdRegex = /^%\d{7}(\d{14})\d{6}$/;
                            const fedExRegex = /^\d{24}$|^\d{34}$/;
                            const dhlExpressRegex = /^JJD\d{9,10}$/;  // Für DHL Express (JJD gefolgt von 9 oder 10 Ziffern)
                            const dhlPaketRegex = /^\d{12,14}$/;  // Für DHL Paket (13-stellige Nummern)
                            const dhlGlobalMailRegex = /^GM\d{9}DE$/;  // Für DHL Global Mail (GM gefolgt von 9 Ziffern und endet mit DE)
                            if (upsRegex.test(brcElement)) shippingCode += `${brcElement}_UPS `;
                            else if (match = brcElement.match(dpdRegex)) shippingCode += `${match[1]}_DPD `;
                            else if (fedExRegex.test(brcElement)) shippingCode += `${brcElement}_FedEx `;
                            else if (dhlExpressRegex.test(brcElement)) shippingCode += `${brcElement}_DHLexp `;
                            else if (dhlGlobalMailRegex.test(brcElement)) shippingCode += `${brcElement}_DHLmail `;
                        })

                        if (element.in.length) {
                            element.in.forEach((link) => {
                                if (isBetDirect(link[0]) && link[0].slice(0, 1) == 'o') {
                                    orderCode = link[0].slice(2).replace(/^0*/, '')
                                    if (orders.has(link[0])) {
                                        customerName = orders.get(link[0])?.comp;
                                        Bemail = orders.get(link[0])?.cem;
                                        BAddress = orders.get(link[0])?.str + ' ' + orders.get(link[0])?.hs;
                                        BZipCode = orders.get(link[0])?.zip;
                                        BCity = orders.get(link[0])?.cit;
                                    }
                                } else if (isBetDirect(link[0]) && tempBet.slice(0, 1) == 'u') {
                                    if (users.has(link[0])) {
                                        customerName = users.get(link[0])?.comp;
                                        Bemail = users.get(link[0])?.cem;
                                        BAddress = users.get(link[0])?.str + ' ' + orders.get(link[0])?.hs;
                                        BZipCode = users.get(link[0])?.zip;
                                        BCity = users.get(link[0])?.cit;
                                    }
                                } else {
                                    customerName = link[0];
                                }
                            })


                        }

                        const uniqueDevices = new Map();

                        element.cont.forEach(dev => {
                            const serialNumber = dev[0]; // Сохраняем оригинальный формат
                            const timestamp = dev[1];

                            // Если устройства с таким номером еще нет или время новее, обновляем Map
                            if (!uniqueDevices.has(serialNumber) || uniqueDevices.get(serialNumber).timestamp < timestamp) {
                                uniqueDevices.set(serialNumber, { timestamp, dev });
                            }
                        });


                        uniqueDevices.forEach(({ dev }) => {


                            const itemTemp = items.get(dev[0]);
                            mcheck = '';
                            mcause = '';
                            mresult = '';
                            mnote = '';
                            itemTemp.actn.forEach(([type, message, timestamp]) => {

                                /*
                                const dict = new Dict("Original text in English");
                                const targetLang = "en";


                                const translatedText = await translateText(dict.translations.original, targetLang);
                                dict.addTranslation(targetLang, translatedText);

                                console.log("Translation added:", dict.getTranslation(targetLang));
*/



                                if (type == 'check') mcheck += message;
                                if (type == 'cause') mcause += message;
                                if (type == 'result') mresult += message;
                                if (type == 'note') mnote += message;

                            });



                            console.log(items.get(dev[0]))
                            if (orderCode.includes('RMA')) {
                                csv += `${dev[0].slice(2).replace(/^0*/, '')};${items.get(dev[0])?.attr?.MN ?? ''};${packIn ? formatUnixTimestamp(packIn).slice(0, 10) : ''};` +
                                    `${packOut ? formatUnixTimestamp(packOut).slice(0, 10) : ''};` +
                                    `${customerName};${items.get(dev[0])?.cl ?? ''};${Bemail};${BAddress};${BZipCode};${BCity};   ;${mcheck};${mcause};${mresult};${shippingCode};${orderCode};` +
                                    `${mnote};   ;   ;   ;    ;   \n`;
                            }
                        });
                    }

                })

                res.writeHead(200, {
                    'Content-Type': 'text/csv',
                    'Content-Disposition': 'attachment; filename="example.csv"',
                    'Content-Length': csv.length
                });
                res.end(csv);




            } else {
                res.statusCode = 200;
                res.setHeader('Content-Type', 'text/html');
                res.end(await parseHtml(parsedData));
            }
        } else {
            // Alle anderen Anfragen -> 404 Not Found
            res.setHeader('Content-Type', 'text/plain');
            res.end('Nicht gefunden');
        }
    } catch (error) {
        // Allgemeine Fehlerbehandlung
        res.setHeader('Content-Type', 'text/plain');
        res.end('Serverfehler: ' + error.message);
    }
};

// Server erstellen und starten
const server = http.createServer({ keepAliveTimeout: 6000 }, requestHandler);


function flipAct(str) {
    const firstChar = str.charAt(0);
    if (firstChar >= 'A' && firstChar <= 'Z') {
        return firstChar.toLowerCase() + str.slice(1); // Großbuchstabe zu Kleinbuchstabe
    } else if (firstChar >= 'a' && firstChar <= 'z') {
        return firstChar.toUpperCase() + str.slice(1); // Kleinbuchstabe zu Großbuchstabe
    }
    return str; // Wenn der erste Buchstabe kein A-Z oder a-z ist, bleibt der String unverändert
}

function disAct(str) {
    return str.charAt(0).toLowerCase() + str.slice(1);
}

function toAct(str) {
    return str.charAt(0).toUpperCase() + str.slice(1);
}

function isAct(str) {
    return /^[A-Z]/.test(str.charAt(0)); // Überprüft, ob der erste Buchstabe ein Großbuchstabe ist
}

function findKnownCode(barcode) {
    if (/^\d{7}$/.test(barcode)) return ('i70000000000' + (barcode));

    return false;
}

function isBetDirect(barcode) {
    // Überprüfen, ob der Barcode ein String ist und die Länge 19 beträgt
    if (typeof barcode === 'string' && barcode.length === 19) {
        // Überprüfen, ob der Barcode mit einem der gültigen Zeichen beginnt
        if (/^[ibpou]/.test(barcode)) {
            return barcode; // Gültiger Barcode, zurückgeben
        }
    }
    return false; // Bedingungen nicht erfüllt
}

initialisation(baseDirectory).then((mes) => {

    writeLog('login  ' + Object.values(process.memoryUsage()));

    classesUpdate();
    upperUpdate();
    //console.log(uppers)
    server.listen(serverPort, () => {
        console.log(`server => localhost:${serverPort}`);
    });




    /*
    
        function readCsvFile(filePath) {
            try {
                // Dateiinhalt synchron lesen
                const csvContent = fs.readFileSync(filePath, "utf-8");
                return csvToJson(csvContent); // CSV zu JSON konvertieren
            } catch (error) {
                console.error(`Error reading the file ${filePath}:`, error.message);
                return [];
            }
        }
    
        function csvToJson(csvString, delimiter = ';') {
            try {
                if (typeof csvString !== "string") {
                    throw new Error("Input must be a string");
                }
    
                const lines = csvString.trim().split("\n");
                if (lines.length < 2) {
                    throw new Error("CSV must have at least a header and one data row");
                }
    
                const headers = lines[0].split(delimiter).map(header => header.trim());
                const jsonArray = lines.slice(1).map(line => {
                    const values = line.split(delimiter).map(value => value.trim());
                    if (values.length !== headers.length) {
                        throw new Error("Row has a different number of columns than the header");
                    }
    
                    return headers.reduce((obj, header, index) => {
                        obj[header] = values[index];
                        return obj;
                    }, {});
                });
    
                return jsonArray;
            } catch (error) {
                console.error("Error converting CSV to JSON:", error.message);
                return [];
            }
        }
    
    
        function dateToUnix(dateString) {
            try {
                // Datum mit `DD/MM/YYYY` parsen
                const [day, month, year] = dateString.split('/').map(Number);
    
                if (!day || !month || !year) {
                    throw new Error("Invalid date format. Use DD/MM/YYYY.");
                }
    
                // Datum in ein `Date`-Objekt umwandeln
                const date = new Date(year, month - 1, day); // Monat ist 0-basiert
    
                if (isNaN(date.getTime())) {
                    throw new Error("Invalid date value.");
                }
    
                // Unix-Timestamp in Sekunden
                return Math.floor(date.getTime() / 1000);
            } catch (error) {
                console.error("Error converting date to Unix time:", error.message);
                return null;
            }
        }
    
    
    
        i7 = readCsvFile("i7.csv");
    
    
        // console.log(dateToUnix(i7[0]['Service Start Date']));
    
        i7.forEach((i7el) => {
    
            if (items.has('i70000000000' + i7el.Serial)) {
                myItem = items.get('i70000000000' + i7el.Serial)
                myItem.cl = i7el.SKU;
                if (!Object.hasOwn(myItem, 'attr')) {
                    myItem.attr = {}; // Initialisiert `attr`, falls es fehlt
                }
                myItem.attr['MN'] = i7el['Model Name'];
                myItem.attr['RN'] = i7el['Reseller Name'];
                myItem.attr['EUN'] = i7el['End User Name'];
                myItem.attr['SSD'] = i7el['Service Start Date'] ? dateToUnix(i7el['Service Start Date']) : 0;
                myItem.attr['SED'] = i7el['Service End Date'] ? dateToUnix(i7el['Service End Date']) : 0;
                myItem.attr['ESD'] = i7el['Extended Start Date'] ? dateToUnix(i7el['Extended Start Date']) : 0;
                myItem.attr['EED'] = i7el['Extended End Date'] ? dateToUnix(i7el['Extended End Date']) : 0;
                console.log(myItem.attr)
                console.log(myItem)
                console.log(Object.keys(myItem)); // Listet alle enumerierbaren Eigenschaften auf
                console.log(Object.getOwnPropertyDescriptors(myItem)); // Zeigt alle Eigenschaften mit Details
            } else {
                console.log('not found', i7el.Serial)
            }
    
        })
    
    */






    /*
        function handleUnknownBarcode(barcode) {
            if (iTem.length) {
                console.log('Es gibt Items im Puffer.');
                const currentItem = items.get(Math.abs(iTem[iTem.length - 1]));
                if (!currentItem.brc) currentItem.brc = [];
                currentItem.brc.push(barcode);
                writeLog(`${barcode} => ${Math.abs(iTem[iTem.length - 1])}`);
            } else if (bOx.length) {
                console.log('Items leer, aber Box-Puffer nicht.');
                const currentBox = boxes.get(Math.abs(bOx[bOx.length - 1]));
                if (!currentBox.brc) currentBox.brc = [];
                currentBox.brc.push(barcode);
                writeLog(`${barcode} => ${Math.abs(bOx[bOx.length - 1])}`);
            } else {
                console.log('Items und Boxes sind leer. Unbekannter Barcode:', barcode);
            }
        }
    */
    items.forEach(tempItem => {
        if (!Object.hasOwn(tempItem, 'attr') && tempItem.sn[0].slice(0, 2) == 'i7') {
            //console.log(tempItem)// Initialisiert `attr`, falls es fehlt
        }
    })


    if (!runOnServer) {






        try {
            const port = new SerialPort({ path: 'com28', baudRate: 115200, autoOpen: true })
            port.setEncoding('utf8')

            console.log('Local mode...')

            port.on('error', (err) => {
                console.error('Scan disabled. Error opening port: ', err.message);
            });

            port.on('readable', () => {
                const barcode = port.read().toString().trim();
                handleBarcode(barcode);

            });











            function handleBarcode(barcode) {

                let bet = '';

                if ((barcode.length === 76 && (bet = betrugerUrlDecrypt(barcode))) || (bet = findKnownCode(barcode)) || (bet = isBetDirect(barcode))) {
                    console.log('betrugerBarcode');
                    const type = bet.slice(0, 1);
                    switch (type) {
                        case 'i': handleItemBarcode(bet); break;
                        case 'b': handleBoxBarcode(bet); break;
                        case 'p': handlePlaceBarcode(bet); break;
                        case 'o': handleOrderBarcode(bet); break;
                        case 'u': handleUserBarcode(bet); break;
                        default: console.warn('Unknown barcode type:', type);
                    }
                } else {
                    console.log('notBetruger');
                    handleUnknownBarcode(barcode);
                }

                console.log(iTem)
                console.log(bOx)
                console.log(pLace)

            }






            function handleItemBarcode(betTemp) {
                if (!items.has(betTemp)) {
                    console.log('create Item ' + betTemp);
                    const tempObj = Object.create(item);
                    tempObj.sn = [betTemp, unixTime()];
                    items.set(betTemp, tempObj);
                }

                if (iTem.length) {
                    var toChangeI;

                    if ((toChangeI = iTem.indexOf(toAct(betTemp))) > -1) {

                        for (i = toChangeI; i < iTem.length; i++) {

                            if (isAct(iTem[i])) {                                         // do someting with choosen active elements
                                iTem[i] = disAct(iTem[i]);
                                console.log('hier ist zu aktiviren das es auf mehrere aktive items gespeichert wird.')

                            }
                        }


                    } else if ((toChangeI = iTem.indexOf(betTemp)) > -1) {

                        iTem[toChangeI] = toAct(betTemp);
                        console.log(iTem)
                    } else {
                        iTem.push(toAct(betTemp))
                        console.log(iTem);
                    }
                } else {
                    iTem.push(toAct(betTemp))
                    console.log(iTem);
                }
            }

            function handleBoxBarcode(betTemp) {
                if (!boxes.has(betTemp)) {
                    console.log('create Box ' + betTemp)
                    const tempObj = Object.create(box);
                    tempObj.sn = [betTemp, unixTime()];
                    boxes.set(betTemp, tempObj);
                }
                if (iTem.length) {                                                      // put item/s to box
                    for (const it of iTem) {
                        addEntryToProperty(items, it, [betTemp, unixTime()], 'loc');
                        addEntryToProperty(boxes, betTemp, [disAct(it), unixTime()], 'cont');
                    }
                    console.log(`(${iTem.length})=> #${betTemp}`);
                    writeLog(`[${iTem}] (${iTem.length})=> b${betTemp}`);
                    iTem.length = 0;
                } else {

                }
                if (bOx.length) {
                    var toChangeB;

                    if ((toChangeB = bOx.indexOf(toAct(betTemp))) > -1) {

                        for (i = toChangeB; i < bOx.length; i++) {

                            if (isAct(bOx[i])) {                                         // do someting with choosen active elements
                                bOx[i] = disAct(bOx[i]);
                                console.log('hier ist zu aktiviren das es auf mehrere aktive bOxs gespeichert wird.')

                            }
                        }


                    } else if ((toChangeB = bOx.indexOf(betTemp)) > -1) {

                        bOx[toChangeB] = toAct(betTemp);
                        console.log(bOx)
                    } else {
                        bOx.push(toAct(betTemp))
                        console.log(bOx);
                    }
                } else {
                    bOx.push(toAct(betTemp))
                    console.log(bOx);
                }


            }


            function handlePlaceBarcode(betTemp) {
                if (!places.has(betTemp)) {
                    console.log('create Place ' + betTemp)
                    const tempObj = Object.create(place);
                    tempObj.sn = [betTemp, unixTime()];
                    places.set(betTemp, tempObj);
                }
                if (iTem.length) {
                    for (const it of iTem) {
                        addEntryToProperty(items, it, [betTemp, unixTime()], 'loc');
                        addEntryToProperty(places, betTemp, [disAct(it), unixTime()], 'cont');
                    }
                    console.log(`(${iTem.length})=> p${betTemp}`);
                    writeLog(`[${iTem}] (${iTem.length})=> p${betTemp}`);
                    iTem.length = 0;
                } else {

                }

                if (bOx.length) {                                                      // put box/es to place

                    for (const it of bOx) {
                        addEntryToProperty(boxes, it, [betTemp, unixTime()], 'loc');
                        addEntryToProperty(places, betTemp, [disAct(it), unixTime()], 'cont');
                    }
                    console.log(`(${bOx.length})=> p${betTemp}`);
                    writeLog(`[${bOx}] (${bOx.length})=> p${betTemp}`);
                    bOx.length = 0;
                } else {

                }


                if (pLace.length) {
                    var toChangeP;

                    if ((toChangeP = pLace.indexOf(toAct(betTemp))) > -1) {

                        for (i = toChangeP; i < pLace.length; i++) {

                            if (isAct(pLace[i])) {                                         // do someting with choosen active elements
                                pLace[i] = disAct(pLace[i]);
                                console.log('hier ist zu aktiviren das es auf mehrere aktive pLaces gespeichert wird.')

                            }
                        }


                    } else if ((toChangeP = pLace.indexOf(betTemp)) > -1) {

                        pLace[toChangeP] = toAct(betTemp);
                        console.log(pLace)
                    } else {
                        pLace.push(toAct(betTemp))
                        console.log(pLace);
                    }
                } else {
                    pLace.push(toAct(betTemp))
                    console.log(pLace);
                }

            }


            function handleOrderBarcode(betTemp) {
                if (!orders.has(betTemp)) {
                    console.log('create Order ' + betTemp)
                    const tempObj = Object.create(order);
                    tempObj.sn = [betTemp, unixTime()];
                    orders.set(betTemp, tempObj);
                }

                if (bOx.length) {                                                      // put box/es to place

                    for (const it of bOx) {
                        addEntryToProperty(boxes, it, [betTemp, unixTime()], 'in');
                        addEntryToProperty(orders, betTemp, [disAct(it), unixTime()], 'cont');
                    }
                    console.log(`(${bOx.length})=> ${betTemp}`);
                    writeLog(`[${bOx}] (${bOx.length})=> ${betTemp}`);
                } else {

                }
            }


            function handleUserBarcode(betTemp) {
                if (!users.has(userTemp)) {
                    console.log('create User ' + userTemp)
                    const tempObj = Object.create(user);
                    tempObj.sn = [userTemp];
                    users.set(userTemp, tempObj);
                }
            }

            function handleUnknownBarcode(barcode) {
                if (iTem.length) {
                    console.log('esGibtItems')                                    // some todo with item or items in itemBuffer

                    if (cla = classes.get(barcode)) {
                        Object.setPrototypeOf(items.get(disAct(iTem[iTem.length - 1])), cla);
                        items.get(disAct(iTem[iTem.length - 1])).cl = barcode;
                        console.log(cla)

                    } else {
                        if (!Object.hasOwn(items.get(disAct(iTem[iTem.length - 1])), 'brc')) items.get(disAct(iTem[iTem.length - 1])).brc = [];
                        items.get(disAct(iTem[iTem.length - 1])).brc.push(barcode);
                        writeLog(`${barcode} => ${disAct(iTem[iTem.length - 1])}`);
                        console.log('write to barcodes ' + barcode)                         // write any unknown barcode to item.brc 

                    }
                } else if (bOx.length) {
                    console.log('itemsAreEmpty but bOx not')                                             // no items in itemBuffer but theis bOx/es
                    if (!Object.hasOwn(boxes.get(disAct(bOx[bOx.length - 1])), 'brc')) boxes.get(disAct(bOx[bOx.length - 1])).brc = [];
                    boxes.get(disAct(bOx[bOx.length - 1])).brc.push(barcode);
                    writeLog(`${barcode} => ${disAct(bOx[bOx.length - 1])}`);
                    console.log('write to barcodes ' + barcode)                         // write any unknown barcode to bOx.brc 

                }


                else {
                    console.log('items and boxex Are Empty')                                             // no items in itemBuffer
                    if (cl = classes.get(barcode)) {
                        console.log(cl)
                    } else {
                        console.log(barcode)   // start with unknown barcode 

                    }

                }
            }

            // Asynchrone Funktion, um Eingaben zu lesen und in einer globalen Variable zu speichern
            async function mainRead() {
                while (true) {
                    userInput = await readFromConsole('');
                    if (/^code\s+\S.*/.test(userInput)) {
                        handleBarcode(userInput.slice(5).trim())
                    } else if (iTem.length) {
                        if (/^\d+(\.\d+)?,\d+(\.\d+)?,\d+(\.\d+)?cm$/.test(userInput)) {
                            addEntryToProperty(items, iTem[iTem.length - 1], [userInput.match(/\d+(\.\d+)?/g).map(Number), unixTime()], 'siz')
                            writeLog(`${userInput} => ${iTem[iTem.length - 1]}.siz`);
                            console.log('write to siz ' + userInput);
                        } else if (/^\d+(\.\d+)?kg$/.test(userInput)) {
                            addEntryToProperty(items, iTem[iTem.length - 1], [parseFloat(userInput.match(/^\d+(\.\d+)?/)[0]), unixTime()], 'mas')
                            writeLog(`${userInput} => ${iTem[iTem.length - 1]}.mas`);
                            console.log('write to mas ' + userInput);
                        } else if (/^desc\s+\S.*/.test(userInput)) {
                            addEntryToProperty(items, iTem[iTem.length - 1], [userInput.slice(5).trim(), unixTime()], 'desc')
                            writeLog(`${userInput.slice(5)} => ${iTem[iTem.length - 1]}.desc`);
                            console.log('write to description ' + userInput.slice(5));
                        } else if (/^(ok|nok)\s+\S.*/i.test(userInput)) {
                            addEntryToProperty(items, iTem[iTem.length - 1], [userInput.trim().toUpperCase(), unixTime()], 'cond')
                            writeLog(`${userInput} => ${iTem[iTem.length - 1]}.cond[ok]`);
                            console.log('write ok to cond ' + userInput);
                        } else if (/^check\s+\S.*/.test(userInput)) {
                            addEntryToProperty(items, iTem[iTem.length - 1], ['check', userInput.slice(6).trim(), unixTime()], 'actn');
                            writeLog(`${userInput.slice(6)} => ${iTem[iTem.length - 1]}.actn[check]`);
                            console.log('write to check ' + userInput.slice(6));
                        } else if (/^cause\s+\S.*/.test(userInput)) {
                            addEntryToProperty(items, iTem[iTem.length - 1], ['cause', userInput.slice(6).trim(), unixTime()], 'actn');
                            writeLog(`${userInput.slice(6)} => ${iTem[iTem.length - 1]}.actn[cause]`);
                            console.log('write to cause ' + userInput.slice(6));
                        }
                        else if (/^result\s+\S.*/.test(userInput)) {
                            addEntryToProperty(items, iTem[iTem.length - 1], ['result', userInput.slice(7).trim(), unixTime()], 'actn');
                            writeLog(`${userInput.slice(7)} => ${iTem[iTem.length - 1]}.actn[result]`);
                            console.log('write to result ' + userInput.slice(7));
                        }
                        else if (/^note\s+\S.*/.test(userInput)) {
                            addEntryToProperty(items, iTem[iTem.length - 1], ['note', userInput.slice(5).trim(), unixTime()], 'actn');
                            writeLog(`${userInput.slice(5)} => ${iTem[iTem.length - 1]}.actn[note]`);
                            console.log('write to note ' + userInput.slice(5));
                        }
                        else if (/^\d+$/.test(userInput)) {
                            addEntryToProperty(items, iTem[iTem.length - 1], [parseInt(userInput), 'i', unixTime()], 'mult')
                            writeLog(`${parseInt(userInput)} => ${iTem[iTem.length - 1]}.mult`);
                            console.log('multiply item activated ' + parseInt(userInput));
                        } else {
                            addEntryToProperty(items, iTem[iTem.length - 1], [userInput, unixTime()], 'desc')
                            writeLog(`${userInput} => ${iTem[iTem.length - 1]}.desc`);
                            console.log('write to description ' + userInput);
                        }

                    } else if (bOx.length) {

                        if (/^\d+(\.\d+)?,\d+(\.\d+)?,\d+(\.\d+)?cm$/.test(userInput)) {
                            addEntryToProperty(boxes, bOx[bOx.length - 1], [userInput.match(/\d+(\.\d+)?/g).map(Number), unixTime()], 'siz')
                            writeLog(`${userInput} => ${bOx[bOx.length - 1]}.siz`);
                            console.log('write to siz ' + userInput);
                        } else if (/^\d+(\.\d+)?kg$/.test(userInput)) {
                            addEntryToProperty(boxes, bOx[bOx.length - 1], [parseFloat(userInput.match(/^\d+(\.\d+)?/)[0]), unixTime()], 'mas')
                            writeLog(`${userInput} => ${bOx[bOx.length - 1]}.mas`);
                            console.log('write to mas ' + userInput);
                        } else if (/^in\s+\S.*/.test(userInput)) {
                            addEntryToProperty(boxes, bOx[bOx.length - 1], [userInput.slice(3).trim(), unixTime()], 'in')
                            writeLog(`${userInput.slice(3)} => ${bOx[bOx.length - 1]}.in`);
                            console.log('write to in ' + userInput.slice(3));
                        } else if (/^desc\s+\S.*/.test(userInput)) {
                            addEntryToProperty(boxes, bOx[bOx.length - 1], [userInput.slice(5).trim(), unixTime()], 'desc')
                            writeLog(`${userInput.slice(5)} => ${bOx[bOx.length - 1]}.desc`);
                            console.log('write to description ' + userInput.slice(5));
                        } else if (/^\d+b$/.test(userInput)) {
                            addEntryToProperty(boxes, bOx[bOx.length - 1], [parseInt(userInput), 'b', unixTime()], 'mult')
                            writeLog(`${parseInt(userInput)} => ${bOx[bOx.length - 1]}.mult`);
                            console.log('multiply bOx activated ' + parseInt(userInput) + 'boxes');
                        } else if (/^\d+$/.test(userInput)) {
                            addEntryToProperty(boxes, bOx[bOx.length - 1], [parseInt(userInput), 'i', unixTime()], 'mult')
                            writeLog(`${parseInt(userInput)} => ${bOx[bOx.length - 1]}.mult`);
                            console.log('multiply bOx activated ' + parseInt(userInput) + 'items');
                        } else if (match = /^replace (b.{18})$/.exec(userInput)) {
                            console.log('hier replace utilite einbauen')
                        } else {
                            addEntryToProperty(boxes, bOx[bOx.length - 1], [userInput, unixTime()], 'desc')
                            writeLog(`${userInput} => ${bOx[bOx.length - 1]}.desc`);
                            console.log('write to description ' + userInput);
                        }
                    }

                }
            }
            mainRead();
        } catch (err) {
            console.error('Critical error initializing SerialPort or Console input:', err.message);
        }

    }






})


/*

const completion = openai.chat.completions.create({
    model: "gpt-4o-mini",
    store: true,
    messages: [
        { role: "system", content: `You are a professional translator. Translate this to ${targetLang}.` },
        { role: "user", content: text }
    ],
});

//completion.then((result) => console.log(result.choices[0].message));
completion.then((result) => {
    console.log(result)
    console.log(result.choices[0].message)
});





//var a = [0x16, 0x4D, 0x0D, 0x3A, 0x2A, 0x3A, 0x48, 0x53, 0x54, 0x41, 0x43, 0x4B, 0x31, 0x2e]
//var a = [0x16, 0x4D, 0x0D, 0x3F, 0x2E]//0x41, 0x43, 0x4B, 0x3F, 0x2e]
//port.write(Buffer.from(a))
//console.log(Buffer.from(a))
*/

/*
setTimeout(() => {
    weiland.forEach((element) => {


        if (!items.has('i70000000000' + element)) console.log('i70000000000' + element)

    })
    console.log("Delayed for 1 second.");

}, 3000);


weiland = [
    2296542,
    2113156,
    2113195,
    1826872,
    2193332,
    1807132,
    1863404,
    2263942,
    1886613,
    1882553,
    2109176,
    2174466,
    1794692,
    2184994,
    2110159,
    1871562,
    2266603,
    2139489,
    2137753,
    2138690,
    1825744,
    1890483,
    2113200,
    1871587,
    1892031,
    2113340,
    2142553,
    1896327,
    1891344,
    1888154,
    1896388,
    1888163,
    1891924,
    2109699,
    2107878,
    1812145,
    1938471,
    1844072,
    1844111,
    1844131,
    1844126,
    1766831,
    1766838,
    1766834,
    2323654,
    1845385,
    1871781,
    1872104,
    2150315,
    2126628,
    2263789,
    2165889,
    1618122,
    2152507,
    1799993,
    1770978,
    1788284,
    1844581,
    1770285,
    2125626,
    1872472,
    1872929,
    2155197,
    2339226,
    1812469,
    1873188,
    1872695,
    1835531,
    1835988,
    1834358,
    1850248,
    1835513,
    1849800,
    1864508,
    1850360,
    2147887,
    2108577,
    1864444,
    2108683,
    1794145,
    1836036,
    1851281,
    1796145,
    1836125,
    1793275,
    1881093,
    1851459,
    1836469,
    1836115,
    1882839,
    2194592,
    1845499,
    2128461,
    2194291,
    2194333
]

*/
