// models/user.js
const Betruger = require('./betruger');
const logger = require('../utils/logging');

/**
 * User model for authentication and authorization
 * @class User
 * @extends Betruger
 */
class User extends Betruger {
  /**
   * Create a new User
   * @param {string} serialNumber - User serial number
   * @param {string} [username] - Username
   * @param {string} [email] - User email
   */
  constructor(serialNumber, username, email) {
    super();
    
    this.sn = [serialNumber, Math.floor(Date.now() / 1000)];
    this.nm = username || '';
    this.cem = email || '';
    this.active = true;
  }
  
  /**
   * Set user password (hashed)
   * @param {string} passwordHash - Hashed password
   * @returns {boolean} Success status
   */
  setPassword(passwordHash) {
    if (!passwordHash) return false;
    
    this.pwd = passwordHash;
    return true;
  }
  
  /**
   * Set user role
   * @param {string} role - User role ('u' for user, 'p' for power user, 'a' for admin)
   * @returns {boolean} Success status
   */
  setRole(role) {
    const validRoles = ['u', 'p', 'a'];
    
    if (!role || !validRoles.includes(role)) {
      return false;
    }
    
    this.r = role;
    return true;
  }
  
  /**
   * Set user company
   * @param {string} company - Company name
   * @returns {boolean} Success status
   */
  setCompany(company) {
    if (!company) return false;
    
    this.comp = company;
    return true;
  }
  
  /**
   * Set user phone
   * @param {string} phone - Phone number
   * @returns {boolean} Success status
   */
  setPhone(phone) {
    this.ph = phone || '';
    return true;
  }
  
  /**
   * Set user street address
   * @param {string} street - Street address
   * @returns {boolean} Success status
   */
  setStreet(street) {
    this.str = street || '';
    return true;
  }
  
  /**
   * Set user house number
   * @param {string} houseNumber - House number
   * @returns {boolean} Success status
   */
  setHouseNumber(houseNumber) {
    this.hs = houseNumber || '';
    return true;
  }
  
  /**
   * Set user postal code
   * @param {string} postalCode - Postal code
   * @returns {boolean} Success status
   */
  setPostalCode(postalCode) {
    this.zip = postalCode || '';
    return true;
  }
  
  /**
   * Set user city
   * @param {string} city - City
   * @returns {boolean} Success status
   */
  setCity(city) {
    this.cit = city || '';
    return true;
  }
  
  /**
   * Set user country
   * @param {string} country - Country
   * @returns {boolean} Success status
   */
  setCountry(country) {
    this.ctry = country || '';
    return true;
  }
  
  /**
   * Update last login timestamp
   * @returns {boolean} Success status
   */
  updateLastLogin() {
    this.lastLogin = Math.floor(Date.now() / 1000);
    return true;
  }
  
  /**
   * Deactivate user
   * @returns {boolean} Success status
   */
  deactivate() {
    this.active = false;
    return true;
  }
  
  /**
   * Activate user
   * @returns {boolean} Success status
   */
  activate() {
    this.active = true;
    return true;
  }
  
  /**
   * Check if user is active
   * @returns {boolean} True if user is active
   */
  isActive() {
    return this.active === true;
  }
  
  /**
   * Check if user is admin
   * @returns {boolean} True if user is admin
   */
  isAdmin() {
    return this.r === 'a';
  }
  
  /**
   * Check if user is power user
   * @returns {boolean} True if user is power user
   */
  isPowerUser() {
    return this.r === 'p' || this.r === 'a';
  }
}

module.exports = User;