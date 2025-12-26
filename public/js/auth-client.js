/**
 * AuthClient - A fetch wrapper that handles JWT authentication and automatic token refreshing
 *
 * Features:
 * - Automatically adds Authorization header to requests
 * - Intercepts 401 Unauthorized responses
 * - Attempts to refresh the access token using the refresh_token
 * - Retries the original request with the new token
 * - Redirects to login if refresh fails
 */

class AuthClient {
    constructor() {
        this.isRefreshing = false;
        this.failedQueue = [];
    }

    /**
     * Get the current access token from localStorage
     */
    getAccessToken() {
        return localStorage.getItem('auth_token');
    }

    /**
     * Get the current refresh token from localStorage
     */
    getRefreshToken() {
        return localStorage.getItem('refresh_token');
    }

    /**
     * Save new tokens to localStorage
     */
    saveTokens(accessToken, refreshToken) {
        localStorage.setItem('auth_token', accessToken);
        if (refreshToken) {
            localStorage.setItem('refresh_token', refreshToken);
        }
    }

    /**
     * Clear all tokens from localStorage
     */
    clearTokens() {
        localStorage.removeItem('auth_token');
        localStorage.removeItem('refresh_token');
    }

    /**
     * Redirect to login page with optional redirect parameter
     */
    redirectToLogin() {
        this.clearTokens();
        window.location.href = '/auth/login?redirect=' + encodeURIComponent(window.location.pathname);
    }

    /**
     * Attempt to refresh the access token using the refresh_token
     * @returns {Promise<string|null>} New access token or null if refresh failed
     */
    async refreshAccessToken() {
        const refreshToken = this.getRefreshToken();

        if (!refreshToken) {
            console.warn('No refresh token available');
            return null;
        }

        try {
            const response = await fetch('/auth/refresh-token', {
                method: 'POST',
                headers: {
                    'Content-Type': 'application/json',
                },
                body: JSON.stringify({ refreshToken: refreshToken })
            });

            if (!response.ok) {
                console.warn('Token refresh failed:', response.status);
                return null;
            }

            const data = await response.json();

            if (data.accessToken) {
                this.saveTokens(data.accessToken, data.refreshToken);
                console.log('Token refreshed successfully');
                return data.accessToken;
            }

            console.warn('No access token in refresh response');
            return null;
        } catch (error) {
            console.error('Error refreshing token:', error);
            return null;
        }
    }

    /**
     * Process queued requests after token refresh
     */
    processQueue(error, token = null) {
        this.failedQueue.forEach(prom => {
            if (error) {
                prom.reject(error);
            } else {
                prom.resolve(token);
            }
        });

        this.failedQueue = [];
    }

    /**
     * Enhanced fetch with automatic token refresh on 401 errors
     * @param {string} url - The URL to fetch
     * @param {Object} options - Fetch options
     * @returns {Promise<Response>}
     */
    async fetch(url, options = {}) {
        const accessToken = this.getAccessToken();

        // Clone options to avoid mutating the original
        const requestOptions = { ...options };
        requestOptions.headers = { ...options.headers };

        // Add Authorization header if token exists and not already present
        if (accessToken && !requestOptions.headers['Authorization']) {
            requestOptions.headers['Authorization'] = `Bearer ${accessToken}`;
        }

        try {
            // Make the initial request
            let response = await fetch(url, requestOptions);

            // If not a 401, return the response
            if (response.status !== 401) {
                return response;
            }

            // Handle 401 - token might be expired
            console.log('Received 401, attempting to refresh token...');

            // If already refreshing, queue this request
            if (this.isRefreshing) {
                return new Promise((resolve, reject) => {
                    this.failedQueue.push({
                        resolve: async (token) => {
                            requestOptions.headers['Authorization'] = `Bearer ${token}`;
                            try {
                                const retryResponse = await fetch(url, requestOptions);
                                resolve(retryResponse);
                            } catch (err) {
                                reject(err);
                            }
                        },
                        reject: (err) => {
                            reject(err);
                        }
                    });
                });
            }

            // Start the refresh process
            this.isRefreshing = true;

            try {
                const newToken = await this.refreshAccessToken();

                if (!newToken) {
                    // Refresh failed - redirect to login
                    this.processQueue(new Error('Token refresh failed'), null);
                    this.redirectToLogin();
                    throw new Error('Session expired. Redirecting to login...');
                }

                // Refresh succeeded - update request and retry
                this.isRefreshing = false;
                this.processQueue(null, newToken);

                requestOptions.headers['Authorization'] = `Bearer ${newToken}`;
                response = await fetch(url, requestOptions);
                return response;

            } catch (refreshError) {
                this.isRefreshing = false;
                this.processQueue(refreshError, null);
                throw refreshError;
            }

        } catch (error) {
            throw error;
        }
    }
}

// Create a global instance
const authClient = new AuthClient();
