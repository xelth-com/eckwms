// models/User.js
const Betruger = require('./Betruger');

class User extends Betruger {
    cont = [];
    constructor(cl) {
        super(cl);
    }
}

module.exports = User;