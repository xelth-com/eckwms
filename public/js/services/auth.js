/**
 * Authentication Service
 * 
 * Handles user authentication and JWT token management on the client side
 */
class AuthService {
  /**
   * Initialize the auth service
   */
  constructor() {
    this.tokenKey = 'jwt';
    this.token = localStorage.getItem(this.tokenKey);
  }

  /**
   * Get the stored JWT token
   * @returns {string|null} JWT token or null if not authenticated
   */
  getToken() {
    return this.token;
  }

  /**
   * Set the JWT token
   * @param {string} token - JWT token
   */
  setToken(token) {
    this.token = token;
    localStorage.setItem(this.tokenKey, token);
  }

  /**
   * Clear the JWT token (logout)
   */
  clearToken() {
    this.token = null;
    localStorage.removeItem(this.tokenKey);
  }

  /**
   * Check if the user is authenticated
   * @returns {boolean} True if authenticated
   */
  isAuthenticated() {
    return !!this.token;
  }

  /**
   * Parse a JWT token without verification
   * @param {string} token - JWT token
   * @returns {Object|null} Decoded token payload or null if invalid
   */
  parseJWT(token) {
    try {
      const parts = token.split('.');
      if (parts.length !== 3) {
        console.error('Invalid JWT format');
        return null;
      }

      const base64Url = parts[1];
      const base64 = base64Url.replace(/-/g, '+').replace(/_/g, '/');
      const jsonPayload = decodeURIComponent(
        atob(base64)
          .split('')
          .map(c => '%' + ('00' + c.charCodeAt(0).toString(16)).slice(-2))
          .join('')
      );

      return JSON.parse(jsonPayload);
    } catch (error) {
      console.error('Error parsing JWT:', error);
      return null;
    }
  }

  /**
   * Verify a JWT token on the client side
   * Note: This only checks expiration, not signature
   * @param {string} token - JWT token
   * @returns {Object} Decoded token payload
   * @throws {Error} If token is invalid or expired
   */
  verifyJWT(token) {
    const parts = token.split('.');
    
    // Check format
    if (parts.length !== 3) {
      throw new Error('Invalid JWT format');
    }
    
    // Decode payload
    try {
      const base64Url = parts[1];
      const base64 = base64Url.replace(/-/g, '+').replace(/_/g, '/');
      const payload = JSON.parse(atob(base64));
      
      // Check expiration
      const currentTime = Math.floor(Date.now() / 1000);
      if (payload.e && currentTime > payload.e) {
        throw new Error('JWT expired');
      }
      
      return payload;
    } catch (error) {
      throw new Error(`Invalid payload format: ${error.message}`);
    }
  }

  /**
   * Get the user role from the JWT token
   * @returns {string|null} User role or null if not authenticated
   */
  getUserRole() {
    if (!this.token) {
      return null;
    }
    
    try {
      const payload = this.verifyJWT(this.token);
      return payload.a || null;
    } catch (error) {
      console.error('Error getting user role:', error);
      return null;
    }
  }

  /**
   * Get the username from the JWT token
   * @returns {string|null} Username or null if not authenticated
   */
  getUsername() {
    if (!this.token) {
      return null;
    }
    
    try {
      const payload = this.verifyJWT(this.token);
      return payload.u || null;
    } catch (error) {
      console.error('Error getting username:', error);
      return null;
    }
  }
}

// Create and export a singleton instance
const authService = new AuthService();
export default authService;