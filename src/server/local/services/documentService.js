const fs = require('fs').promises;
const path = require('path');
const { Parser } = require('json2csv');

const storagePath = path.join(__dirname, '../../../../storage');

async function handleManualRestockOrder(payload) {
  if (!Array.isArray(payload)) {
    throw new Error('Payload for ManualRestockOrder must be an array.');
  }

  const fields = ['barcode', 'quantity', 'note'];
  const json2csvParser = new Parser({ fields });
  const csv = json2csvParser.parse(payload);

  const timestamp = new Date().toISOString().replace(/[:.]/g, '-');
  const filename = `restock-order_${timestamp}.csv`;
  const directory = path.join(storagePath, 'restock_orders');

  await fs.mkdir(directory, { recursive: true });
  const filePath = path.join(directory, filename);

  await fs.writeFile(filePath, csv);
  console.log(`[DocumentService] Saved manual restock order to ${filePath}`);
  return { filePath, filename };
}

async function processDocument(type, payload) {
  switch (type) {
    case 'ManualRestockOrder':
      return await handleManualRestockOrder(payload);
    // Add other document types here in the future
    // case 'InventoryCheck':
    //   return await handleInventoryCheck(payload);
    default:
      throw new Error(`Unsupported document type: ${type}`);
  }
}

module.exports = { processDocument };
