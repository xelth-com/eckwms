const { Item, Box, Place } = require('../../../shared/models/postgresql');

class InventoryService {
    _getModel(type) {
        switch (type) {
            case 'item': return Item;
            case 'box': return Box;
            case 'place': return Place;
            default: throw new Error(`Unknown type: ${type}`);
        }
    }

    async exists(type, id) {
        const model = this._getModel(type);
        const count = await model.count({ where: { id } });
        return count > 0;
    }

    async get(type, id) {
        const model = this._getModel(type);
        const record = await model.findByPk(id);
        if (!record) return null;
        // Merge top-level ID with JSONB data to reconstruct legacy object structure
        const data = record.data || {};
        return { ...data, id: record.id, class: record.class };
    }

    async create(type, id, data) {
        const model = this._getModel(type);
        const classCode = data.cl || null;
        await model.create({
            id,
            class: classCode,
            data
        });
        return data;
    }

    /**
     * Updates specific fields in the data JSONB
     */
    async update(type, id, updates) {
        const model = this._getModel(type);
        const record = await model.findByPk(id);
        if (!record) throw new Error(`${type} ${id} not found`);

        const newData = { ...record.data, ...updates };
        await record.update({
            data: newData,
            class: updates.cl || record.class
        });
        return newData;
    }

    /**
     * Pushes a value to an array field in the JSONB data (e.g., 'loc', 'cont', 'actn')
     */
    async pushToArray(type, id, field, value) {
        const record = await this.get(type, id);
        if (!record) throw new Error(`${type} ${id} not found`);

        const array = record[field] || [];
        array.push(value);

        await this.update(type, id, { [field]: array });
        return array;
    }

    async getAll(type) {
        const model = this._getModel(type);
        const records = await model.findAll();
        return records.map(r => ({ ...r.data, id: r.id, class: r.class }));
    }

    async queryJson(type, filterFn) {
        // Note: For performance on large datasets, specific SQL queries should be used instead of this
        // This is a compatibility layer for complex legacy filtering
        const all = await this.getAll(type);
        return all.filter(filterFn);
    }
}

module.exports = new InventoryService();
