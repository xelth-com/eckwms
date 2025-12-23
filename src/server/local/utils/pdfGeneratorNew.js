// utils/pdfGeneratorNew.js - Using pdf-lib instead of pdfmake
const { PDFDocument, StandardFonts, rgb } = require('pdf-lib');
const QRCode = require('qrcode');
const { betrugerUrlEncrypt, base32table } = require('../../../shared/utils/encryption');
const crc32 = require('buffer-crc32');

/**
 * Helper: Draws a QR code as vector rectangles onto a PDF page
 * @param {PDFPage} page - The page to draw on
 * @param {string} text - The data to encode
 * @param {number} x - X coordinate (bottom-left)
 * @param {number} y - Y coordinate (bottom-left)
 * @param {number} size - Width/Height of the QR code
 * @param {Object} rgbColor - Color object (default black)
 */
function drawQrVector(page, text, x, y, size, rgbColor) {
    const qr = QRCode.create(text, { errorCorrectionLevel: 'M' });
    const matrix = qr.modules;
    const cellSize = size / matrix.size;

    // Draw black modules
    for (let r = 0; r < matrix.size; r++) {
        for (let c = 0; c < matrix.size; c++) {
            if (matrix.get(r, c)) {
                page.drawRectangle({
                    x: x + (c * cellSize),
                    // Flip Y axis: PDF coords start at bottom-left, QR matrix starts top-left
                    y: y + size - ((r + 1) * cellSize),
                    width: cellSize,
                    height: cellSize,
                    color: rgbColor,
                });
            }
        }
    }
}

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

        // --- VECTOR QR DRAWING START ---
        // Draw label content (QR1, field1, QR2, field2, QR3) using vector primitives
        let contentOffsetX = x + 5;

        // Draw QR1 (vector)
        drawQrVector(currentPage, `ECK1.COM/${code}${INSTANCE_SUFFIX}`, contentOffsetX, currentY + 7, qrSize, rgb(0, 0, 0));
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

        // Draw QR2 (vector)
        drawQrVector(currentPage, `ECK2.COM/${code}${INSTANCE_SUFFIX}`, contentOffsetX, currentY + 7, qrSize, rgb(0, 0, 0));
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

        // Draw QR3 (vector)
        drawQrVector(currentPage, `ECK3.COM/${code}${INSTANCE_SUFFIX}`, contentOffsetX, currentY + 7, qrSize, rgb(0, 0, 0));
        // --- VECTOR QR DRAWING END ---
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

/**
 * Generates a PDF for an RMA request using pdf-lib
 * @param {Object} rmaJs - RMA data
 * @param {string} link - Tracking link
 * @param {string} token - JWT token
 * @param {string} code - Betruger code
 * @returns {Promise<Buffer>} - PDF buffer
 */
async function generatePdfRma(rmaJs, link, token, code) {
    console.log('[PDF-LIB] Starting RMA PDF generation');

    const INSTANCE_SUFFIX = process.env.INSTANCE_SUFFIX || 'M3';

    // Create a new PDF document
    const pdfDoc = await PDFDocument.create();
    const font = await pdfDoc.embedFont(StandardFonts.Helvetica);
    const fontBold = await pdfDoc.embedFont(StandardFonts.HelveticaBold);

    // A4 page dimensions
    const pageWidth = 595;
    const pageHeight = 842;

    // --- PAGE 1 ---
    const page1 = pdfDoc.addPage([pageWidth, pageHeight]);
    let yPos = pageHeight - 50;

    // Header: RMA code
    page1.drawText(rmaJs.rma, {
        x: 50,
        y: yPos,
        size: 22,
        font: fontBold,
        color: rgb(0, 0, 0)
    });
    yPos -= 40;

    // Three-column layout: Customer address, QR code (link), M3 address
    const leftColX = 50;
    const centerColX = 230;
    const rightColX = 380;

    // Left column: Customer address
    let leftY = yPos;
    if (rmaJs.company) {
        page1.drawText(rmaJs.company, { x: leftColX, y: leftY, size: 12, font: fontBold, color: rgb(0, 0, 0) });
        leftY -= 15;
    }
    if (rmaJs.person) {
        page1.drawText(rmaJs.person, { x: leftColX, y: leftY, size: 12, font: font, color: rgb(0, 0, 0) });
        leftY -= 15;
    }
    if (rmaJs.street) {
        page1.drawText(rmaJs.street, { x: leftColX, y: leftY, size: 12, font: font, color: rgb(0, 0, 0) });
        leftY -= 15;
    }
    if (rmaJs.postal) {
        page1.drawText(rmaJs.postal, { x: leftColX, y: leftY, size: 12, font: font, color: rgb(0, 0, 0) });
        leftY -= 15;
    }
    if (rmaJs.country) {
        page1.drawText(rmaJs.country, { x: leftColX, y: leftY, size: 12, font: font, color: rgb(0, 0, 0) });
    }

    // Center column: QR code with tracking link (Vector)
    drawQrVector(page1, link, centerColX, yPos - 100, 110, rgb(0, 0, 0));

    // Right column: M3 address
    let rightY = yPos;
    page1.drawText('M3 Mobile GmbH', { x: rightColX, y: rightY, size: 12, font: fontBold, color: rgb(0, 0, 0) });
    rightY -= 15;
    page1.drawText('Am Holzweg 26', { x: rightColX, y: rightY, size: 12, font: font, color: rgb(0, 0, 0) });
    rightY -= 15;
    page1.drawText('65830 Kriftel', { x: rightColX, y: rightY, size: 12, font: font, color: rgb(0, 0, 0) });
    rightY -= 15;
    page1.drawText('Deutschland', { x: rightColX, y: rightY, size: 12, font: font, color: rgb(0, 0, 0) });

    yPos -= 140;

    // Contact info section
    let contactY = yPos;
    if (rmaJs.email) {
        page1.drawText(`Contact Email: ${rmaJs.email}`, { x: leftColX, y: contactY, size: 12, font: fontBold, color: rgb(0, 0, 0) });
        contactY -= 15;
    }
    if (rmaJs.invoice_email) {
        page1.drawText(`E-Invoice Email: ${rmaJs.invoice_email}`, { x: leftColX, y: contactY, size: 12, font: fontBold, color: rgb(0, 0, 0) });
        contactY -= 15;
    }
    if (rmaJs.phone) {
        page1.drawText(`Phone: ${rmaJs.phone}`, { x: leftColX, y: contactY, size: 12, font: fontBold, color: rgb(0, 0, 0) });
        contactY -= 15;
    }
    if (rmaJs.resellerName) {
        page1.drawText(`Reseller: ${rmaJs.resellerName}`, { x: leftColX, y: contactY, size: 12, font: fontBold, color: rgb(0, 0, 0) });
        contactY -= 15;
    }

    // QR code with JWT token (right side) (Vector)
    drawQrVector(page1, token, rightColX, yPos - 90, 100, rgb(0, 0, 0));

    yPos = contactY - 20;

    // Tracking link
    page1.drawText('RMA Tracking Link:', { x: leftColX, y: yPos, size: 12, font: font, color: rgb(0, 0, 0) });
    yPos -= 15;
    page1.drawText(link, { x: leftColX, y: yPos, size: 10, font: font, color: rgb(0, 0, 0.53) });
    yPos -= 25;

    // Access token
    page1.drawText('RMA Access Token for full access:', { x: leftColX, y: yPos, size: 12, font: font, color: rgb(0, 0, 0) });
    yPos -= 15;

    // Split token into multiple lines if too long
    const tokenChunkSize = 60;
    for (let i = 0; i < token.length; i += tokenChunkSize) {
        const chunk = token.substring(i, i + tokenChunkSize);
        page1.drawText(chunk, { x: leftColX, y: yPos, size: 10, font: font, color: rgb(0, 0.4, 0) });
        yPos -= 12;
    }

    yPos -= 15;

    // Serial numbers section
    page1.drawText('Serial Numbers and Issue Descriptions', { x: leftColX, y: yPos, size: 18, font: fontBold, color: rgb(0, 0, 0) });
    yPos -= 25;

    // List serial numbers and descriptions
    for (let i = 1; i <= 30; i++) {
        const serialKey = `serial${i}`;
        const descriptionKey = `description${i}`;

        if (rmaJs[serialKey] && rmaJs[descriptionKey]) {
            // Serial number
            page1.drawText(rmaJs[serialKey], { x: leftColX, y: yPos, size: 10, font: font, color: rgb(0, 0, 0) });

            // Description (wrap if too long)
            const description = rmaJs[descriptionKey];
            const maxDescWidth = 350;
            page1.drawText(description.substring(0, 80), { x: leftColX + 100, y: yPos, size: 10, font: font, color: rgb(0, 0, 0) });

            yPos -= 15;

            // Break if we run out of space
            if (yPos < 50) break;
        }
    }

    // --- PAGE 2 ---
    const page2 = pdfDoc.addPage([pageWidth, pageHeight]);

    // M3 address in letter window position (absolute positioning)
    const windowX = 70;
    const windowY = pageHeight - 150;

    let winY = windowY;
    page2.drawText('M3 Mobile GmbH', { x: windowX, y: winY, size: 12, font: fontBold, color: rgb(0, 0, 0) });
    winY -= 15;
    page2.drawText('Am Holzweg 26', { x: windowX, y: winY, size: 12, font: font, color: rgb(0, 0, 0) });
    winY -= 15;
    page2.drawText('65830 Kriftel', { x: windowX, y: winY, size: 12, font: font, color: rgb(0, 0, 0) });
    winY -= 15;
    page2.drawText('Deutschland', { x: windowX, y: winY, size: 12, font: font, color: rgb(0, 0, 0) });

    // QR code next to address (Vector)
    drawQrVector(page2, `ECK1.COM/${code}${INSTANCE_SUFFIX}`, 180, windowY - 45, 58, rgb(0, 0, 0));

    // Instruction text and QR codes
    let instrY = windowY - 100;
    page2.drawText('Please send us only this sheet or write', { x: 50, y: instrY, size: 14, font: font, color: rgb(0.53, 0, 0) });
    instrY -= 18;
    page2.drawText(`${rmaJs.rma} on the parcel.`, { x: 50, y: instrY, size: 14, font: fontBold, color: rgb(0.53, 0, 0) });

    // QR codes on the right (Vector)
    drawQrVector(page2, `ECK2.COM/${code}${INSTANCE_SUFFIX}`, 420, instrY - 30, 58, rgb(0, 0, 0));
    drawQrVector(page2, `ECK3.COM/${code}${INSTANCE_SUFFIX}`, 490, instrY - 30, 58, rgb(0, 0, 0));

    // Additional QR codes row (Vector)
    instrY -= 80;
    drawQrVector(page2, `ECK1.COM/${code}${INSTANCE_SUFFIX}`, 350, instrY, 58, rgb(0, 0, 0));
    drawQrVector(page2, `ECK2.COM/${code}${INSTANCE_SUFFIX}`, 420, instrY, 58, rgb(0, 0, 0));
    drawQrVector(page2, `ECK3.COM/${code}${INSTANCE_SUFFIX}`, 490, instrY, 58, rgb(0, 0, 0));

    // Dotted line
    instrY -= 70;
    const dashPattern = [5, 5];
    for (let x = 15; x < 495; x += 10) {
        page2.drawLine({
            start: { x, y: instrY },
            end: { x: x + 5, y: instrY },
            thickness: 1,
            color: rgb(0, 0, 0)
        });
    }

    // Serial numbers section on page 2
    instrY -= 30;
    page2.drawText('Serial Numbers and Issue Descriptions', { x: 50, y: instrY, size: 18, font: fontBold, color: rgb(0, 0, 0) });
    instrY -= 25;

    // List serial numbers and descriptions again
    for (let i = 1; i <= 30; i++) {
        const serialKey = `serial${i}`;
        const descriptionKey = `description${i}`;

        if (rmaJs[serialKey] && rmaJs[descriptionKey]) {
            page2.drawText(rmaJs[serialKey], { x: 50, y: instrY, size: 10, font: font, color: rgb(0, 0, 0) });

            const description = rmaJs[descriptionKey];
            page2.drawText(description.substring(0, 80), { x: 150, y: instrY, size: 10, font: font, color: rgb(0, 0, 0) });

            instrY -= 15;

            if (instrY < 50) break;
        }
    }

    // Generate PDF buffer
    const pdfBytes = await pdfDoc.save();
    console.log('[PDF-LIB] RMA PDF generation completed:', { bufferSize: pdfBytes.length });

    return Buffer.from(pdfBytes);
}

module.exports = {
    betrugerPrintCodesPdf,
    generatePdfRma
};
