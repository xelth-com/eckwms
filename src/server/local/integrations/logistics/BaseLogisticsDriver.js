class BaseLogisticsDriver {
    constructor(config) {
        this.config = config;
    }

    get name() {
        throw new Error('Method not implemented');
    }

    /**
     * Validate address data before creating shipment
     * @param {Object} addressData
     * @returns {Promise<boolean>}
     */
    async validateAddress(addressData) {
        return true;
    }

    /**
     * Create a shipment order
     * @param {Object} shipmentData - Standardized shipment object
     * @returns {Promise<Object>} - Result { status, trackingNumber, labelUrl, message, internalRef }
     */
    async createShipment(shipmentData) {
        throw new Error('Method not implemented');
    }

    /**
     * Get current tracking status
     * @param {string} trackingNumber
     * @returns {Promise<Object>}
     */
    async getTrackingStatus(trackingNumber) {
        throw new Error('Method not implemented');
    }

    /**
     * Cancel a shipment
     * @param {string} trackingNumber
     */
    async cancelShipment(trackingNumber) {
        throw new Error('Method not implemented');
    }
}

module.exports = BaseLogisticsDriver;
