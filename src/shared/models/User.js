// models/User.js
const Eck = require('./Eck');

class User extends Eck {
    cont = [];
    constructor(cl) {
        super(cl);
    }
}

module.exports = User;