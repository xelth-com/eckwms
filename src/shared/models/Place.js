// models/Place.js
const Eck = require('./Eck');

class Place extends Eck {
    cont = [];
    constructor(cl) {
        super(cl);
    }
}

module.exports = Place;