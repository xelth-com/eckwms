// models/index.js
const Eck = require('./Eck');
const Betruger = require('./Betruger'); // Legacy support
const User = require('./User');
const Order = require('./Order');
const Place = require('./Place');
const Box = require('./Box');
const Item = require('./Item');
const Dict = require('./Dict');

module.exports = {
    Eck,
    Betruger, // Legacy support
    User,
    Order,
    Place,
    Box,
    Item,
    Dict
};