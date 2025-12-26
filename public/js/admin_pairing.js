document.addEventListener('DOMContentLoaded', () => {
    const generateBtn = document.getElementById('generate-qr-btn');
    const qrContainer = document.getElementById('qr-container');
    const qrImg = document.getElementById('qr-code-img');
    const statusBox = document.getElementById('status-box');

    // Generate QR Code
    generateBtn.addEventListener('click', async () => {
        qrContainer.innerHTML = '<p>Generating QR code...</p>';
        qrImg.style.display = 'none';

        try {
            const isVip = document.getElementById('vip-mode-check').checked;
            const url = isVip ? '/api/internal/pairing-qr?type=vip' : '/api/internal/pairing-qr';

            const response = await authClient.fetch(url);

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

// --- Device Management Logic ---

async function loadDevices() {
    const container = document.getElementById('devices-list');

    try {
        const response = await authClient.fetch('/admin/api/devices');

        if (!response.ok) {
            throw new Error(`Server returned status ${response.status}`);
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
            actions += `<button onclick="deleteDevice('${device.deviceId}', this)" class="btn-action btn-delete">🗑️</button>`;

            html += `
                <tr data-id="${device.deviceId}">
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

// --- UX Helpers ---
function showToast(message, onUndo, duration = 5000) {
    const container = document.getElementById('toast-container') || createToastContainer();
    const toast = document.createElement('div');
    toast.className = 'toast';

    const msgSpan = document.createElement('span');
    msgSpan.className = 'toast-message';
    msgSpan.textContent = message;

    const undoBtn = document.createElement('button');
    undoBtn.className = 'toast-undo-btn';
    undoBtn.textContent = '⟲ UNDO';

    // Progress bar
    const progressBar = document.createElement('div');
    progressBar.className = 'toast-progress';
    progressBar.style.animation = `shrink ${duration}ms linear`;

    let isUndone = false;
    let timer = setTimeout(() => {
        if (!isUndone) {
            toast.style.animation = 'fadeOut 0.3s ease-out';
            setTimeout(() => {
                toast.remove();
                onUndo(false); // Timer finished, execute action
            }, 300);
        }
    }, duration);

    undoBtn.onclick = () => {
        isUndone = true;
        clearTimeout(timer);
        toast.style.animation = 'fadeOut 0.3s ease-out';
        setTimeout(() => toast.remove(), 300);
        onUndo(true); // User clicked Undo
    };

    toast.appendChild(msgSpan);
    toast.appendChild(undoBtn);
    toast.appendChild(progressBar);
    container.appendChild(toast);

    // Add shrink animation to CSS dynamically
    if (!document.getElementById('toast-animations')) {
        const style = document.createElement('style');
        style.id = 'toast-animations';
        style.textContent = `
            @keyframes shrink {
                from { transform: scaleX(1); }
                to { transform: scaleX(0); }
            }
            @keyframes fadeOut {
                from { opacity: 1; transform: translateY(0); }
                to { opacity: 0; transform: translateY(20px); }
            }
        `;
        document.head.appendChild(style);
    }
}

function createToastContainer() {
    const div = document.createElement('div');
    div.id = 'toast-container';
    div.className = 'toast-container';
    document.body.appendChild(div);
    return div;
}

// --- Actions ---
window.updateStatus = async (id, status) => {
    const row = document.querySelector(`tr[data-id='${id}']`);
    if (row) row.style.opacity = '0.5'; // Visual feedback

    showToast(`Changing status to ${status.toUpperCase()}...`, async (isUndo) => {
        if (isUndo) {
            if (row) row.style.opacity = '1';
            return;
        }

        // Execute API call
        await authClient.fetch(`/admin/api/devices/${id}/status`, {
            method: 'POST',
            headers: {
                'Content-Type': 'application/json'
            },
            body: JSON.stringify({ status })
        });
        loadDevices();
    }, 4000); // 4 seconds to undo
};

let deleteTimeouts = {};
window.deleteDevice = async (id, btn) => {
    if (btn.classList.contains('btn-confirm-delete')) {
        // Real delete
        await authClient.fetch(`/admin/api/devices/${id}`, {
            method: 'DELETE'
        });
        loadDevices();
        return;
    }

    // First click - Change to confirm state
    const originalText = btn.innerHTML;
    btn.innerHTML = 'Sure?';
    btn.classList.add('btn-confirm-delete');

    // Reset after 3 seconds if not clicked
    if (deleteTimeouts[id]) clearTimeout(deleteTimeouts[id]);
    deleteTimeouts[id] = setTimeout(() => {
        btn.innerHTML = originalText;
        btn.classList.remove('btn-confirm-delete');
    }, 3000);
};

// Load devices on startup and refresh every 10s
loadDevices();
setInterval(loadDevices, 10000);
