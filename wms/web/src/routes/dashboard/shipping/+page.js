import { api } from "$lib/api";

/** @type {import('./$types').PageLoad} */
export async function load() {
    try {
        const [pickings, shipments, providersConfig] = await Promise.all([
            api.get("/api/odoo/pickings?state=assigned"),
            api.get("/api/delivery/shipments"),
            api.get("/api/delivery/config")
        ]);

        return {
            pickings: pickings || [],
            shipments: shipments || [],
            providersConfig: providersConfig || { opal: false, dhl: false }
        };
    } catch (e) {
        console.error("Load error:", e);
        return {
            pickings: [],
            shipments: [],
            providersConfig: { opal: false, dhl: false },
            error: e.message
        };
    }
}
