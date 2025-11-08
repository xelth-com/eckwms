// models/Item.js
const Betruger = require('./Betruger');

class Item extends Betruger {
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