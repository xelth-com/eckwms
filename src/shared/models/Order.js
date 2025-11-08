// models/Order.js
const Betruger = require('./Betruger');

class Order extends Betruger {
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