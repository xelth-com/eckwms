// In-app document viewer state. A kiosk browser has no tab bar / back button, so
// a file opened with window.open() traps the operator. Instead openFile() puts
// the blob here and the dashboard layout shows it INSIDE the WMS frame with a
// Back button. { url, name, mime } | null
import { writable } from 'svelte/store';

export const docViewer = writable(null);

export function closeDocViewer() {
    docViewer.update((cur) => {
        if (cur?.url) {
            try { URL.revokeObjectURL(cur.url); } catch { /* already gone */ }
        }
        return null;
    });
}
