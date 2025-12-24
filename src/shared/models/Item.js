// models/Item.js
const Eck = require('./Eck');

class Item extends Eck {
    loc = [];
    prc = [];
    mult = [];
    up = [];
    down = [];
    attr = {};
    prop = [['material'], ['color']];
    rel = [['partOf'], ['purpose'], ['consistOf']];
    constructor(cl) {
        super(cl);
    }
}

module.exports = Item;