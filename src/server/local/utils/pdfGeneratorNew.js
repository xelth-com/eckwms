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
 * Generates PDF with dynamic layout configuration
 * @param {string} codeType - Type of code: 'i', 'b', 'p', 'l'
 * @param {number} startNumber - Starting number for codes
 * @param {Object} config - Layout configuration (margins, gaps, cols, rows in pts)
 * @param {number} count - Total labels to generate
 * @returns {Promise<Buffer>} - PDF buffer
 */
async function betrugerPrintCodesPdf(codeType, startNumber = 0, config = {}, count = null) {
    // Default layout (in points, 1mm ~ 2.83pt)
    const layout = {
        cols: config.cols || 2,
        rows: config.rows || 8,
        marginTop: config.marginTop !== undefined ? config.marginTop : 20,
        marginBottom: config.marginBottom !== undefined ? config.marginBottom : 20,
        marginLeft: config.marginLeft !== undefined ? config.marginLeft : 20,
        marginRight: config.marginRight !== undefined ? config.marginRight : 20,
        gapX: config.gapX !== undefined ? config.gapX : 10,
        gapY: config.gapY !== undefined ? config.gapY : 0
    };

    console.log('[PDF-LIB] Generating PDF with layout:', layout);

    const INSTANCE_SUFFIX = process.env.INSTANCE_SUFFIX || 'M3';
    let totalLabels = count || (layout.cols * layout.rows);

    const pdfDoc = await PDFDocument.create();
    const font = await pdfDoc.embedFont(StandardFonts.Helvetica);
    const fontBold = await pdfDoc.embedFont(StandardFonts.HelveticaBold);

    // A4 Dimensions in Points
    const pageWidth = 595.28;
    const pageHeight = 841.89;

    // Calculate dimensions of a single label
    // Working area = Total size - margins
    const workingWidth = pageWidth - layout.marginLeft - layout.marginRight;
    const workingHeight = pageHeight - layout.marginTop - layout.marginBottom;

    // Total gaps = (count - 1) * gapSize
    const totalGapWidth = (layout.cols - 1) * layout.gapX;
    const totalGapHeight = (layout.rows - 1) * layout.gapY;

    // Label size = (Working area - total gaps) / count
    const labelWidth = (workingWidth - totalGapWidth) / layout.cols;
    const labelHeight = (workingHeight - totalGapHeight) / layout.rows;

    let currentPage = null;

    for (let i = 0; i < totalLabels; i++) {
        const labelIndex = i + startNumber;
        const labelsPerPage = layout.cols * layout.rows;
        const indexOnPage = i % labelsPerPage;

        // Add page if needed
        if (i % labelsPerPage === 0) {
            currentPage = pdfDoc.addPage([pageWidth, pageHeight]);
        }

        // Grid position (0-indexed)
        const col = indexOnPage % layout.cols;
        const row = Math.floor(indexOnPage / layout.cols);

        // X coordinate (from left)
        const x = layout.marginLeft + (col * (labelWidth + layout.gapX));

        // Y coordinate (from BOTTOM as per PDF-LIB)
        // Y of the top edge of the current row = pageHeight - marginTop - (row * labelHeight) - (row * gapY)
        // Y of the bottom edge (where we draw) = topEdge - labelHeight
        const y = pageHeight - layout.marginTop - ((row + 1) * labelHeight) - (row * layout.gapY);

        // Content generation
        let code, field1, field2;
        const serialDigits = config.serialDigits || 0;

        // Helper to format serial number based on serialDigits setting (full 18-digit support)
        const formatSerial = (num, prefix = '', fullPadding = 18) => {
            const numStr = num.toString();
            const paddedFull = ('0'.repeat(fullPadding) + numStr).slice(-fullPadding);

            if (serialDigits > 0) {
                // Show only last N digits from the full 18-digit padded number
                return prefix + paddedFull.slice(-serialDigits);
            } else {
                // Show full 18-digit padded number
                return prefix + paddedFull;
            }
        };

        if (codeType === 'i' || codeType === 'b' || codeType === 'l') {
            code = betrugerUrlEncrypt(`${codeType}${('000000000000000000' + labelIndex).slice(-18)}`);
            const temp = crc32.unsigned(labelIndex.toString()) & 1023;
            field2 = Buffer.from([base32table[temp >> 5], base32table[temp & 31]]).toString();

            if (codeType === 'i') field1 = formatSerial(labelIndex, '', 18);
            else if (codeType === 'b') field1 = formatSerial(labelIndex, '#', 18);
            else if (codeType === 'l') field1 = formatSerial(labelIndex, 'L', 18);
        } else if (codeType === 'p') {
            // Places
            code = betrugerUrlEncrypt(`p${('000000000000000000' + (labelIndex + 1)).slice(-18)}`);
            const paddedNum = ('000000000000000000' + (labelIndex + 1)).slice(-18);
            field1 = serialDigits > 0 ? `P-${paddedNum.slice(-serialDigits)}` : `P-${labelIndex + 1}`;
            field2 = "00";
        }

        const cCfg = config.contentConfig || null;

        // Element positioning helper (X/Y are 0-100% of label size)
        const drawElement = (type, cfg, fallback) => {
            const scale = cfg.scale !== undefined ? cfg.scale : fallback.scale;
            const xOff = (cfg.x !== undefined ? cfg.x : fallback.x) * (labelWidth / 100);
            const yOff = (cfg.y !== undefined ? cfg.y : fallback.y) * (labelHeight / 100);

            if (type.startsWith('qr')) {
                const qrSize = Math.min(labelHeight, labelWidth) * scale;
                const qrText = type === 'qr1' ? `ECK1.COM/${code}${INSTANCE_SUFFIX}` :
                    type === 'qr2' ? `ECK2.COM/${code}${INSTANCE_SUFFIX}` :
                        `ECK3.COM/${code}${INSTANCE_SUFFIX}`;
                drawQrVector(currentPage, qrText, x + xOff, y + yOff, qrSize, rgb(0, 0, 0));
            } else if (type === 'checksum') {
                const fSize = Math.min(labelHeight, labelWidth) * scale;
                currentPage.drawText(field2, {
                    x: x + xOff,
                    y: y + yOff,
                    size: fSize,
                    font: fontBold,
                    color: rgb(0, 0, 0)
                });
            } else if (type === 'serial') {
                const fSize = Math.min(labelHeight, labelWidth) * scale;
                currentPage.drawText(field1, {
                    x: x + xOff,
                    y: y + yOff,
                    size: fSize,
                    font: font,
                    color: rgb(0.2, 0.2, 0.2)
                });
            }
        };

        if (!cCfg) {
            // Default "Master QR" Puzzle Layout (Left-to-Right logic)
            // QR1: Large (Master)
            const qr1Scale = 0.9;
            const qr1Size = labelHeight * qr1Scale;
            drawQrVector(currentPage, `ECK1.COM/${code}${INSTANCE_SUFFIX}`, x + 2, y + (labelHeight - qr1Size) / 2, qr1Size, rgb(0, 0, 0));

            // Checksum (Large center)
            const csScale = 0.45;
            const csSize = labelHeight * csScale;
            currentPage.drawText(field2, {
                x: x + qr1Size + 10,
                y: y + (labelHeight / 2) - (csSize / 4),
                size: csSize,
                font: fontBold,
                color: rgb(0, 0, 0)
            });
            const csWidth = fontBold.widthOfTextAtSize(field2, csSize);

            // Serial (Small below checksum)
            const sScale = 0.15;
            const sSize = labelHeight * sScale;
            currentPage.drawText(field1, {
                x: x + qr1Size + 10,
                y: y + (labelHeight * 0.15),
                size: sSize,
                font: font,
                color: rgb(0.3, 0.3, 0.3)
            });

            // QR2 & QR3 (Small on right)
            const sQrScale = 0.35;
            const sQrSize = labelHeight * sQrScale;
            const rightX = x + qr1Size + csWidth + 20;
            if (rightX + sQrSize < x + labelWidth) {
                drawQrVector(currentPage, `ECK2.COM/${code}${INSTANCE_SUFFIX}`, rightX, y + labelHeight - sQrSize - 5, sQrSize, rgb(0, 0, 0));
                drawQrVector(currentPage, `ECK3.COM/${code}${INSTANCE_SUFFIX}`, rightX, y + 5, sQrSize, rgb(0, 0, 0));
            }
        } else {
            // Manual configuration from UI
            drawElement('qr1', cCfg.qr1 || {}, { scale: 0.8, x: 5, y: 10 });
            drawElement('qr2', cCfg.qr2 || {}, { scale: 0.3, x: 75, y: 50 });
            drawElement('qr3', cCfg.qr3 || {}, { scale: 0.3, x: 75, y: 10 });
            drawElement('checksum', cCfg.checksum || {}, { scale: 0.5, x: 40, y: 35 });
            drawElement('serial', cCfg.serial || {}, { scale: 0.2, x: 40, y: 10 });
        }

        // Draw debug border (optional)
        // currentPage.drawRectangle({ x, y, width: labelWidth, height: labelHeight, borderColor: rgb(0.8, 0.8, 0.8), borderWidth: 0.5 });
    }

    const pdfBytes = await pdfDoc.save();
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
