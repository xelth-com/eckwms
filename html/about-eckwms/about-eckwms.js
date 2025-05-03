// html/about-eckwms/about-eckwms.js
import { loadCSS, loadTemplate } from '/core/module-loader.js';
export async function init(container) {
    console.log('[AboutEckWMS] Initializing module...');
    try {
        // Load CSS first - this will now resolve even if CSS fails
        await loadCSS('/about-eckwms/about-eckwms.css');
        // Load HTML template - this will resolve with empty string if fails
        const html = await loadTemplate('/about-eckwms/about-eckwms.template.html');

        if (html) {
            container.innerHTML = html;
            console.log('[AboutEckWMS] HTML template loaded.');

            // Translate the content immediately after inserting
            if (window.i18n && typeof window.i18n.translateElement === 'function') {
                console.log('[AboutEckWMS] Attempting translation...');
                // Указываем namespace, чтобы i18n.js знал, какой файл перевода искать
                window.i18n.translateElement(container, { namespace: 'about_eckwms' });
                console.log('[AboutEckWMS] Translation attempt complete.');
            } else {
                console.warn("[AboutEckWMS] i18n function 'translateElement' not available.");
            }
        } else {
            // Handle case where template failed to load
            console.error('[AboutEckWMS] HTML template failed to load. Displaying error message.');
            container.innerHTML = `<p style="color: red;">Failed to load About eckWMS content.</p>`;
        }
    } catch (error) {
        // Catch any unexpected errors during init
        console.error('[AboutEckWMS] Unexpected error during initialization:', error);
        container.innerHTML = <p style="color: red;">An unexpected error occurred while loading this page.</p>;
    }
}
export function postInit() {
    console.log("[AboutEckWMS] Module post-initialized.");
    // Add any specific JS interactions for this page here if needed
}