document.addEventListener('DOMContentLoaded', () => {
    const generateBtn = document.getElementById('generate-qr-btn');
    const qrContainer = document.getElementById('qr-container');
    const qrImg = document.getElementById('qr-code-img');
    const statusBox = document.getElementById('status-box');

    // Function to get JWT from localStorage
    function getAuthToken() {
        return localStorage.getItem('auth_token');
    }

    // Generate QR Code
    generateBtn.addEventListener('click', async () => {
        qrContainer.innerHTML = '<p>Generating QR code...</p>';
        qrImg.style.display = 'none';

        const token = getAuthToken();
        if (!token) {
            qrContainer.innerHTML = '<p style="color:red;">Error: You are not authenticated. Please log in again.</p>';
            return;
        }

        try {
            const response = await fetch('/api/internal/pairing-qr', {
                headers: {
                    'Authorization': `Bearer ${token}`
                }
            });

            if (!response.ok) {
                throw new Error(`Server returned status ${response.status}`);
            }

            const data = await response.json();
            qrImg.src = data.qr_code_data_url;
            qrImg.style.display = 'block';
            qrContainer.innerHTML = '';
            qrContainer.appendChild(qrImg);
        } catch (error) {
            console.error('Error generating QR code:', error);
            qrContainer.innerHTML = `<p style="color:red;">Failed to generate QR code: ${error.message}</p>`;
        }
    });

    // Check Global Server Status
    async function checkGlobalServerStatus() {
        try {
            const response = await fetch('/api/internal/global-server-status');
            const data = await response.json();

            if (data.status === 'online') {
                statusBox.className = 'status-box status-online';
                statusBox.textContent = `Global Server is ONLINE.`;
            } else {
                statusBox.className = 'status-box status-offline';
                statusBox.textContent = `Global Server is OFFLINE. Error: ${data.error}`;
            }
        } catch (error) {
            statusBox.className = 'status-box status-offline';
            statusBox.textContent = `Could not reach local API to check global status: ${error.message}`;
        }
    }

    // Initial check and periodic refresh
    checkGlobalServerStatus();
    setInterval(checkGlobalServerStatus, 15000); // Check every 15 seconds
});
