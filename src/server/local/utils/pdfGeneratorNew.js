// utils/pdfGeneratorNew.js - Using pdf-lib instead of pdfmake
const { PDFDocument, StandardFonts, rgb } = require('pdf-lib');
const QRCode = require('qrcode');
const { betrugerUrlEncrypt, base32table } = require('../../../shared/utils/encryption');
const crc32 = require('buffer-crc32');

/**
 * Generates PDF files with Betruger codes using pdf-lib
 * @param {string} codeType - Type of code: 'i' for items, 'b' for boxes, 'p' for places, 'l' for InBody markers
 * @param {number} startNumber - Starting number for codes
 * @param {Array} arrDim - Array dimensions [['', cols], ['', rows]]
 * @param {number} count - Number of labels to generate
 * @param {number} cols - Number of columns per page (default: 2)
 * @param {number} rows - Number of rows per page (default: 16)
 * @returns {Promise<Buffer>} - PDF buffer
 */
async function betrugerPrintCodesPdf(codeType, startNumber = 0, arrDim = [], count = null, cols = 2, rows = 16) {
    console.log('[PDF-LIB] Starting PDF generation:', { codeType, startNumber, count, cols, rows });

    const INSTANCE_SUFFIX = process.env.INSTANCE_SUFFIX || 'M3';

    // Determine total labels
    let totalLabels = count || 32;
    if (codeType === 'i') totalLabels = count || 32;
    else if (codeType === 'b') totalLabels = count || 16;
    else if (codeType === 'p') totalLabels = count || 32;
    else if (codeType === 'l') totalLabels = count || 16;

    // Create a new PDF document
    const pdfDoc = await PDFDocument.create();
    const font = await pdfDoc.embedFont(StandardFonts.Helvetica);
    const fontBold = await pdfDoc.embedFont(StandardFonts.HelveticaBold);

    // A4 page dimensions
    const pageWidth = 595;
    const pageHeight = 842;

    // Calculate label dimensions
    const labelsPerRow = 2;
    const labelWidth = pageWidth / labelsPerRow;
    const labelHeight = 44;

    // QR code settings
    const qrSize = 29;
    const qrMargin = 2;

    let currentPage = null;
    let currentY = 10;

    // Generate labels
    for (let i = 0; i < totalLabels; i++) {
        const labelIndex = i + startNumber;

        // Create new page if needed
        if (i % (labelsPerRow * rows) === 0) {
            currentPage = pdfDoc.addPage([pageWidth, pageHeight]);
            currentY = pageHeight - 10 - labelHeight;
        }

        const col = i % labelsPerRow;
        const x = col * labelWidth + 10;

        // Move to next row
        if (col === 0 && i > 0 && i % labelsPerRow === 0) {
            currentY -= labelHeight;
        }

        // Generate code and data
        let code, field1, field2;

        if (codeType === 'i' || codeType === 'b' || codeType === 'l') {
            code = betrugerUrlEncrypt(`${codeType}${('000000000000000000' + labelIndex).slice(-18)}`);
            const temp = crc32.unsigned(labelIndex.toString()) & 1023;
            field2 = Buffer.from([base32table[temp >> 5], base32table[temp & 31]]).toString();

            if (codeType === 'i') {
                field1 = `${('000000' + labelIndex).slice(-6)}`;
            } else if (codeType === 'b') {
                field1 = `#${('00000' + labelIndex).slice(-5)}`;
            } else if (codeType === 'l') {
                field1 = `L${('00000' + labelIndex).slice(-5)}`;
            }
        } else if (codeType === 'p') {
            const place00 = labelIndex % arrDim[0][1];
            const place01 = Math.floor(labelIndex / arrDim[0][1]) % arrDim[1][1];
            code = betrugerUrlEncrypt(`${codeType}${('000000000000000000' + (labelIndex + 1)).slice(-18)}`);
            field1 = `${arrDim[1][0]}${place01 + 1}`;
            field2 = `${arrDim[0][0]}${place00 + 1}`;
        }

        // Generate QR codes as PNG buffers
        const qr1Buffer = await QRCode.toBuffer(`ECK1.COM/${code}${INSTANCE_SUFFIX}`, {
            width: qrSize * 3,
            margin: 0,
            type: 'png'
        });
        const qr2Buffer = await QRCode.toBuffer(`ECK2.COM/${code}${INSTANCE_SUFFIX}`, {
            width: qrSize * 3,
            margin: 0,
            type: 'png'
        });
        const qr3Buffer = await QRCode.toBuffer(`ECK3.COM/${code}${INSTANCE_SUFFIX}`, {
            width: qrSize * 3,
            margin: 0,
            type: 'png'
        });

        // Embed QR codes as PNG
        const qr1Image = await pdfDoc.embedPng(qr1Buffer);
        const qr2Image = await pdfDoc.embedPng(qr2Buffer);
        const qr3Image = await pdfDoc.embedPng(qr3Buffer);

        // Draw label content (QR1, field1, QR2, field2, QR3)
        const contentX = x + 5;
        let contentOffsetX = contentX;

        // QR1
        currentPage.drawImage(qr1Image, {
            x: contentOffsetX,
            y: currentY + 7,
            width: qrSize,
            height: qrSize
        });
        contentOffsetX += qrSize + qrMargin;

        // field1
        currentPage.drawText(field1, {
            x: contentOffsetX,
            y: currentY + labelHeight / 2 - 3,
            size: 20,
            font: fontBold,
            color: rgb(0, 0, 0)
        });
        const field1Width = fontBold.widthOfTextAtSize(field1, 20);
        contentOffsetX += field1Width + qrMargin + 5;

        // QR2
        currentPage.drawImage(qr2Image, {
            x: contentOffsetX,
            y: currentY + 7,
            width: qrSize,
            height: qrSize
        });
        contentOffsetX += qrSize + qrMargin;

        // field2
        currentPage.drawText(field2, {
            x: contentOffsetX,
            y: currentY + labelHeight / 2 - 5,
            size: 25,
            font: fontBold,
            color: rgb(0, 0, 0)
        });
        const field2Width = fontBold.widthOfTextAtSize(field2, 25);
        contentOffsetX += field2Width + qrMargin + 5;

        // QR3
        currentPage.drawImage(qr3Image, {
            x: contentOffsetX,
            y: currentY + 7,
            width: qrSize,
            height: qrSize
        });
    }

    // Generate PDF buffer
    const pdfBytes = await pdfDoc.save();
    console.log('[PDF-LIB] PDF generation completed:', {
        codeType,
        startNumber,
        totalLabels,
        bufferSize: pdfBytes.length
    });

    return Buffer.from(pdfBytes);
}

module.exports = {
    betrugerPrintCodesPdf
};
