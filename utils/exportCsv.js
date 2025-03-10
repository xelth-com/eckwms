const fs = require('fs');
const { Parser } = require('json2csv');

function exportToCsv(data, fields, filename) {
  const json2csvParser = new Parser({ fields });
  const csv = json2csvParser.parse(data);

  fs.writeFileSync(filename, csv);
  console.log(`Exported data to ${filename}`);
}

module.exports = exportToCsv;