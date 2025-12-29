const express = require('express');
const router = express.Router();
const multer = require('multer');
const path = require('path');
const fs = require('fs');
const crc32 = require('buffer-crc32');
const { requireAuth } = require('../middleware/auth');

// Configure multer for file storage
const storage = multer.diskStorage({
  destination: function (req, file, cb) {
    const uploadPath = path.join(__dirname, '../html/storage/uploads');
    // Create directory if it doesn't exist
    fs.mkdirSync(uploadPath, { recursive: true });
    cb(null, uploadPath);
  },
  filename: function (req, file, cb) {
    const uniqueSuffix = Date.now() + '-' + Math.round(Math.random() * 1E9);
    cb(null, file.fieldname + '-' + uniqueSuffix + path.extname(file.originalname));
  }
});

// SECURITY FIX: Add file type validation and size limits
const upload = multer({
  storage: storage,
  limits: {
    fileSize: 10 * 1024 * 1024 // 10MB max file size
  },
  fileFilter: (req, file, cb) => {
    // Only allow image files
    const allowedTypes = /jpeg|jpg|png|webp|avif/;
    const extname = allowedTypes.test(path.extname(file.originalname).toLowerCase());
    const mimetype = allowedTypes.test(file.mimetype);

    if (mimetype && extname) {
      return cb(null, true);
    } else {
      cb(new Error('Only image files are allowed (jpeg, jpg, png, webp, avif)'));
    }
  }
});

// POST /api/upload/image
// SECURITY FIX: Authentication re-enabled - all uploads require authentication
router.post('/image', requireAuth, upload.single('image'), (req, res) => {
  if (!req.file) {
    return res.status(400).json({ success: false, message: 'No image file uploaded.' });
  }

  const deviceId = req.body.deviceId || 'unknown-device';
  const scanMode = req.body.scanMode || 'unknown-mode';
  const barcodeData = req.body.barcodeData || null;
  const imageChecksum = req.body.imageChecksum || null;

  // Validate checksum if provided
  if (imageChecksum) {
    try {
      const buffer = fs.readFileSync(req.file.path);
      const calculatedChecksum = crc32.unsigned(buffer).toString(16).padStart(8, '0');

      if (calculatedChecksum.toLowerCase() !== imageChecksum.toLowerCase()) {
        console.error(`[Upload] Checksum mismatch for ${req.file.filename}. Client: ${imageChecksum}, Server: ${calculatedChecksum}`);
        // Delete the corrupted file
        fs.unlinkSync(req.file.path);
        return res.status(400).json({ success: false, message: 'Checksum mismatch. File may be corrupted.' });
      }
      console.log(`[Upload] Checksum validated successfully for ${req.file.filename}`);
    } catch (err) {
      console.error(`[Upload] Error during checksum validation: ${err.message}`);
      return res.status(500).json({ success: false, message: 'Error during file validation.' });
    }
  }

  console.log(`[Upload] Received image for device ${deviceId} in '${scanMode}' mode.`);
  if (barcodeData) {
    console.log(`[Upload] ML Kit barcode data: ${barcodeData}`);
  }

  // Construct the publicly accessible URL
  const imageUrl = `/storage/uploads/${req.file.filename}`;

  res.status(200).json({
    success: true,
    message: 'Image uploaded successfully',
    url: imageUrl,
    filename: req.file.filename,
    scanMode: scanMode,
    barcodeData: barcodeData
  });
});

module.exports = router;