// html/terms/terms.js
import { loadCSS, loadTemplate } from '/core/module-loader.js';
export async function init(container) {
await loadCSS('/terms/terms.css'); // Load CSS if it exists
const html = await loadTemplate('/terms/terms.template.html');
container.innerHTML = html;
// Translate the content immediately after inserting, specifying the namespace
if (window.i18n && typeof window.i18n.translateElement === 'function') {
window.i18n.translateElement(container, { namespace: 'terms' });
} else {
console.warn("i18n function not available for terms page translation");
}
}
export function postInit() {
// No specific post-init logic needed for this static page yet
console.log("Terms & Conditions module post-initialized.");
}