// html/privacy/privacy.js
import { loadCSS, loadTemplate } from '/core/module-loader.js';
export async function init(container) {
await loadCSS('/privacy/privacy.css'); // Load CSS if it exists
const html = await loadTemplate('/privacy/privacy.template.html');
container.innerHTML = html;
// Translate the content immediately after inserting, specifying the namespace
if (window.i18n && typeof window.i18n.translateElement === 'function') {
window.i18n.translateElement(container, { namespace: 'privacy' });
} else {
console.warn("i18n function not available for privacy page translation");
}
}
export function postInit() {
// No specific post-init logic needed for this static page yet
console.log("Privacy Policy module post-initialized.");
}