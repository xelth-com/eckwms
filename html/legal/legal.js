// html/legal/legal.js
import { loadCSS, loadTemplate } from '/core/module-loader.js';
export async function init(container) {
await loadCSS('/legal/legal.css'); // Load CSS if it exists
const html = await loadTemplate('/legal/legal.template.html');
container.innerHTML = html;
// Translate the content immediately after inserting, specifying the namespace
if (window.i18n && typeof window.i18n.translateElement === 'function') {
window.i18n.translateElement(container, { namespace: 'legal' });
} else {
console.warn("i18n function not available for legal page translation");
}
}
export function postInit() {
// No specific post-init logic needed for this static page yet
console.log("Legal Notice module post-initialized.");
}