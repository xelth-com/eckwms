// eckWMS Client Application
// This file will contain client-side API calls and UI interactions

console.log('eckWMS client application loaded');

// Placeholder for future API integration
const api = {
    baseUrl: window.location.origin,

    async fetchStatus() {
        try {
            const response = await fetch(`${this.baseUrl}/api/status`);
            return await response.json();
        } catch (error) {
            console.error('Failed to fetch status:', error);
            return null;
        }
    }
};

// Initialize application when DOM is ready
document.addEventListener('DOMContentLoaded', () => {
    console.log('DOM loaded, initializing eckWMS client...');

    // Future initialization code will go here
});
