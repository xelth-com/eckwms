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
            const isVip = document.getElementById('vip-mode-check').checked;
            const url = isVip ? '/api/internal/pairing-qr?type=vip' : '/api/internal/pairing-qr';

            const response = await fetch(url, {
                headers: {
                    'Authorization': `Bearer ${token}`
                }
            });

            if (!response.ok) {
                // Handle 401 Unauthorized - token expired or invalid
                if (response.status === 401) {
                    localStorage.removeItem('auth_token');
                    window.location.href = '/auth/login';
                    return;
                }
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

// --- Device Management Logic ---

async function loadDevices() {
    const container = document.getElementById('devices-list');
    const token = localStorage.getItem('auth_token');

    try {
        const response = await fetch('/admin/api/devices', {
            headers: { 'Authorization': `Bearer ${token}` }
        });

        if (response.status === 401 || response.status === 403) {
            console.warn('Auth failed, redirecting to login');
            window.location.href = '/auth/login';
            return;
        }

        const devices = await response.json();

        if (!Array.isArray(devices)) {
             throw new Error(devices.error || 'Invalid server response (not an array)');
        }

        if (devices.length === 0) {
            container.innerHTML = '<p>No devices registered yet.</p>';
            return;
        }

        let html = `
            <table class="device-table">
                <thead>
                    <tr>
                        <th>Status</th>
                        <th>Device ID / Name</th>
                        <th>Last Seen</th>
                        <th>Actions</th>
                    </tr>
                </thead>
                <tbody>
        `;

        devices.forEach(device => {
            const statusClass = `badge-${device.status}`;
            const name = device.deviceName || 'Unknown Device';
            const lastSeen = new Date(device.updatedAt).toLocaleString();

            let actions = '';
            if (device.status === 'pending') {
                actions += `<button onclick="updateStatus('${device.deviceId}', 'active')" class="btn-action btn-approve">✅ Approve</button>`;
            }
            if (device.status === 'active') {
                actions += `<button onclick="updateStatus('${device.deviceId}', 'blocked')" class="btn-action btn-block">⛔ Block</button>`;
            }
            if (device.status === 'blocked') {
                actions += `<button onclick="updateStatus('${device.deviceId}', 'active')" class="btn-action btn-approve">🔄 Unblock</button>`;
            }
            actions += `<button onclick="deleteDevice('${device.deviceId}')" class="btn-action btn-delete">🗑️</button>`;

            html += `
                <tr>
                    <td><span class="badge ${statusClass}">${device.status}</span></td>
                    <td>
                        <strong>${device.deviceId.substring(0, 16)}...</strong><br>
                        <small>${name}</small>
                    </td>
                    <td>${lastSeen}</td>
                    <td>${actions}</td>
                </tr>
            `;
        });

        html += '</tbody></table>';
        container.innerHTML = html;

    } catch (error) {
        container.innerHTML = `<p style="color:red">Error loading devices: ${error.message}</p>`;
    }
}

// Make functions available globally for onclick handlers
window.updateStatus = async (id, status) => {
    if (!confirm(`Change status to ${status}?`)) return;
    const token = localStorage.getItem('auth_token');
    await fetch(`/admin/api/devices/${id}/status`, {
        method: 'POST',
        headers: {
            'Content-Type': 'application/json',
            'Authorization': `Bearer ${token}`
        },
        body: JSON.stringify({ status })
    });
    loadDevices(); // Refresh table
};

window.deleteDevice = async (id) => {
    if (!confirm('Delete this device permanently?')) return;
    const token = localStorage.getItem('auth_token');
    await fetch(`/admin/api/devices/${id}`, {
        method: 'DELETE',
        headers: { 'Authorization': `Bearer ${token}` }
    });
    loadDevices(); // Refresh table
};

// Load devices on startup and refresh every 10s
loadDevices();
setInterval(loadDevices, 10000);
