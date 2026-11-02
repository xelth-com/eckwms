// Open an auth-gated file (PDF / document) in the in-app viewer.
//
// `GET /api/files/:id` sits behind auth_middleware and only accepts a Bearer
// token in the Authorization header — a plain `<a href>` or `window.open(url)`
// gets a 401. So we fetch the bytes with the token (same source AuthImg uses:
// the authStore, with a localStorage fallback), turn them into an object URL
// and show THAT in the viewer overlay.
import { get } from 'svelte/store';
import { authStore } from '$lib/stores/authStore';
import { docViewer, closeDocViewer } from '$lib/stores/docViewer';

// Same token source as AuthImg.svelte's `tokenOf()`.
function tokenOf() {
    const s = get(authStore);
    return (
        s?.token ||
        (typeof localStorage !== 'undefined' && localStorage.getItem('auth_token')) ||
        null
    );
}

/**
 * Fetch `url` with the Bearer token and show the result in the in-app viewer
 * (see stores/docViewer.js + the dashboard layout) - never in a new window: the
 * kiosk browser has no tabs or back button. Returns true on success, throws on a
 * failed fetch. The object URL is revoked when the viewer is closed.
 */
export async function openFile(url, name = '') {
    const token = tokenOf();
    const resp = await fetch(url, {
        headers: token ? { Authorization: `Bearer ${token}` } : {}
    });
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    let blob = await resp.blob();
    // The server may label a PDF application/octet-stream; sniff the magic so the
    // viewer frame renders it instead of offering a download.
    if (!blob.type || blob.type === 'application/octet-stream') {
        const head = new TextDecoder().decode(await blob.slice(0, 5).arrayBuffer());
        if (head.startsWith('%PDF')) blob = new Blob([blob], { type: 'application/pdf' });
    }
    closeDocViewer(); // drop a previous one (revokes its URL)
    docViewer.set({ url: URL.createObjectURL(blob), name, mime: blob.type || '' });
    return true;
}
