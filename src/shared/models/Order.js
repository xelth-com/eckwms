// models/Order.js
const Eck = require('./Eck');

class Order extends Eck {
    cust = [];
    comp = '';
    pers = '';
    str = '';
    hs = '';
    zip = '';
    ctry = '';
    cit = '';
    cem = '';
    iem = '';
    ph = '';
    cont = [];
    decl = [];

    constructor(cl) {
        super(cl);
    }
}

module.exports = Order;