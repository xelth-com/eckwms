// models/Box.js
const Eck = require('./Eck');

class Box extends Eck {
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