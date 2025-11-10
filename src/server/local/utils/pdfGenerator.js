// utils/pdfGenerator.js
const PdfPrinter = require('pdfmake');
const { betrugerCrc, base32table } = require('../../../shared/utils/encryption');
const fs = require('fs');
const path = require('path');

/**
 * Generates PDF files with Betruger codes
 * @param {string} codeType - Type of code: 'i' for items, 'b' for boxes, 'p' for places
 * @param {number} startNumber - Starting number for codes
 * @param {Array} arrDim - Array dimensions [['', cols], ['', rows]]
 * @returns {void}
 */
function betrugerPrintCodesPdf(codeType, startNumber = 0, arrDim = []) {
    const fonts = {
        Roboto: {
            normal: path.join(__dirname, '../fonts/Roboto-Regular.ttf')
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
    } else if (codeType === 'b') {
        body.forEach((element, index) => {
            const index1 = index + startNumber;
            const codeTemp1 = `${betrugerUrlEncrypt(`${codeType}${('000000000000000000' + (index1)).slice(-18)}`)}`;
            const temp1 = crc32.unsigned(index1.toString()) & 1023;
            const field2Temp1 = Buffer.from([base32table[temp1 >> 5], base32table[temp1 & 31]]).toString();
            dd.content[0].table.body.push([labelMake(codeTemp1, `#${('00000' + index1).slice(-5)}`, field2Temp1), labelMake(codeTemp1, `#${('00000' + index1).slice(-5)}`, field2Temp1)]);
        });
    } else if (codeType === 'p') {
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
    }

    var options = {}
    var pdfDoc = printer.createPdfKitDocument(dd, options);
    pdfDoc.pipe(fs.createWriteStream(`eckwms_${codeType}${startNumber}.pdf`));
    pdfDoc.end();
}

/**
 * Generates a PDF for an RMA request
 * @param {Object} rmaJs - RMA data
 * @param {string} link - Tracking link
 * @param {string} token - JWT token
 * @param {string} code - Betruger code
 * @returns {Promise<Buffer>} - PDF buffer
 */
function generatePdfRma(rmaJs, link, token, code) {
    return new Promise((resolve, reject) => {
        const fonts = {
            Roboto: {
                normal: path.join(__dirname, '../fonts/Roboto-Regular.ttf'),
                bold: path.join(__dirname, '../fonts/Roboto-Medium.ttf'),
                italics: path.join(__dirname, '../fonts/Roboto-Italic.ttf'),
                bolditalics: path.join(__dirname, '../fonts/Roboto-MediumItalic.ttf'),
            },
        };

        try {
            const dd = {
                content: [
                    // First page
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
                                qr: link, // QR Code with tracking link
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
                                qr: token, // QR Code with JWT
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
                        link: link, // Clickable
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
                    // Dynamically generated serial numbers and descriptions
                    ...Object.keys(rmaJs)
                        .filter((key) => key.startsWith('serial') && rmaJs[key]) // Only use Serial fields
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
                    // Second page
                    {
                        text: '',
                        pageBreak: 'before', // New page
                    },
                    {
                        text: [
                            { text: 'M3 Mobile GmbH\n', bold: true },
                            'Am Holzweg 26\n',
                            '65830 Kriftel\n',
                            'Deutschland\n',
                        ],
                        absolutePosition: { x: 70, y: 150 }, // Position in letter window
                        fontSize: 12,
                    },
                    {
                        qr: `ECK1.COM/${code}M3`,
                        fit: '58',
                        absolutePosition: { x: 180, y: 150 }, // Position in letter window
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
                            // First dotted line (1/3 of A4)
                            {
                                type: 'line',
                                x1: 15,
                                y1: 50,
                                x2: 495,
                                y2: 50,
                                lineWidth: 1,
                                dash: { length: 5 }, // Dotted line
                            },
                        ],
                    },
                    {
                        text: 'Serial Numbers and Issue Descriptions',
                        style: 'subheader',
                        margin: [0, 30, 0, 10],
                    },
                    // Dynamically generated serial numbers and descriptions
                    ...Object.keys(rmaJs)
                        .filter((key) => key.startsWith('serial') && rmaJs[key]) // Only use Serial fields
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

module.exports = {
    betrugerPrintCodesPdf,
    generatePdfRma
};