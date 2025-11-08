// models/Place.js
const Betruger = require('./Betruger');

class Place extends Betruger {
    cont = [];
    constructor(cl) {
        super(cl);
    }
}

module.exports = Place;