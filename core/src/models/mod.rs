pub mod sync_packet;
pub mod odoo_types;
pub mod action_proof;

// === WMS Models (stripped from Sea-ORM) ===
pub mod product;
pub mod location;
pub mod user;
pub mod order;
pub mod document;
pub mod file_resource;
pub mod attachment;
pub mod item;
pub mod order_item_event;
pub mod stock_picking_delivery;
pub mod mesh_node;
pub mod registered_device;
pub mod checksum;
pub mod quant;
pub mod picking;
pub mod move_line;
pub mod rack;
pub mod partner;
pub mod product_alias;
pub mod delivery_carrier;
pub mod delivery_tracking;
pub mod sync_history;
pub mod device_intake;
pub mod inventory_discrepancy;

// === POS Models (stripped from Sea-ORM) ===
pub mod pos;

// === Relay Models ===
pub mod relay;
