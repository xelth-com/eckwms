// models/Box.js
const Betruger = require('./Betruger');

class Box extends Betruger {
    in = [];
    out = [];
    cont = [];
    mult = [];
    loc = [];
    prc = [];
    constructor(cl) {
        super(cl);
    }
}

module.exports = Box;