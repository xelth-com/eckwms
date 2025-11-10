// services/rmaService.js
const { RmaRequest } = require('../../../shared/models/postgresql');
const { betrugerCrc } = require('../../../shared/utils/encryption');

/**
 * Создает новый RMA-запрос
 * @param {Object} data - Данные RMA-запроса
 * @returns {Promise<Object>} - Созданный RMA-запрос
 */
async function createRmaRequest(data) {
  try {
    // Генерация уникального RMA кода, если не предоставлен
    let rmaCode = data.rmaCode;
    if (!rmaCode) {
      const timestamp = Math.floor(Date.now() / 1000);
      rmaCode = `RMA${timestamp}${betrugerCrc(timestamp)}`;
    }
    
    // Создание записи в базе данных
    const newRma = await RmaRequest.create({
      userId: data.userId || null,
      rmaCode: rmaCode,
      orderCode: data.orderCode || `o000${rmaCode}`,
      status: 'created',
      company: data.company,
      person: data.person || null,
      street: data.street,
      houseNumber: data.houseNumber || null,
      postalCode: data.postalCode,
      city: data.city,
      country: data.country,
      email: data.email,
      invoiceEmail: data.invoiceEmail || null,
      phone: data.phone || null,
      resellerName: data.resellerName || null,
      devices: data.devices || [],
      orderData: data.orderData || null
    });
    
    return newRma;
  } catch (error) {
    console.error('Error creating RMA:', error);
    throw error;
  }
}

/**
 * Получает RMA-запрос по коду
 * @param {string} rmaCode - Код RMA
 * @returns {Promise<Object>} - RMA-запрос
 */
async function getRmaRequestByCode(rmaCode) {
  return RmaRequest.findOne({ where: { rmaCode } });
}

// Другие функции для работы с RMA...

module.exports = {
  createRmaRequest,
  getRmaRequestByCode
  // Экспорт других функций...
};