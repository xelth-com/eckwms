// html/js/eckwms-feed.js
(function() {
    const feedContainer = document.getElementById('scan-feed');
    const pollingInterval = 2000; // 2 seconds

    async function fetchScans() {
        try {
            const response = await fetch('/eckwms/api/scans');
            if (!response.ok) {
                throw new Error(`HTTP error! status: ${response.status}`);
            }
            const scans = await response.json();
            renderScans(scans);
        } catch (error) {
            console.error('Error fetching scans:', error);
            feedContainer.innerHTML = '<p style="color: red;">Error loading scan feed. Retrying...</p>';
        }
    }

    function renderScans(scans) {
        if (!scans || scans.length === 0) {
            feedContainer.innerHTML = '<p>No scans received yet. Try scanning with the eckwms-movFast app!</p>';
            return;
        }

        let html = '';
        scans.forEach(scan => {
            const scanDate = new Date(scan.createdAt).toLocaleString();
            const barcode = scan.payload || 'N/A';
            const scanType = scan.type || 'N/A';
            html += `
                <div class="scan-item">
                    <div class="barcode">${escapeHtml(barcode)}</div>
                    <div class="meta">
                        Type: ${escapeHtml(scanType)}<br>
                        Time: ${scanDate}
                    </div>
                </div>
            `;
        });

        feedContainer.innerHTML = html;
    }

    function escapeHtml(unsafe) {
        if (!unsafe) return '';
        return unsafe
             .replace(/&/g, "&amp;")
             .replace(/</g, "&lt;")
             .replace(/>/g, "&gt;")
             .replace(/"/g, "&quot;")
             .replace(/'/g, "&#039;");
    }

    // Start polling
    setInterval(fetchScans, pollingInterval);

    // Initial fetch
    fetchScans();
})();
