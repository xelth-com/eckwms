//! POS-specific models (stripped from Sea-ORM)

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use surrealdb::types::SurrealValue;
use uuid::Uuid;

/// Company / tenant (SurrealDB document)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Company {
    pub id: i32,
    pub company_full_name: String,
    pub meta_information: serde_json::Value,
    pub global_configurations: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Branch within a company (SurrealDB document)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Branch {
    pub id: i32,
    pub company_id: i32,
    pub branch_name: String,
    pub branch_address: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// POS device (SurrealDB document)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PosDevice {
    pub id: i32,
    pub branch_id: i32,
    pub pos_device_name: String,
    pub pos_device_type: String,
    pub pos_device_external_number: i32,
    pub pos_device_settings: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Role for POS auth (SurrealDB document)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Role {
    pub id: i32,
    pub role_name: String,
    pub role_display_names: serde_json::Value,
    pub description: Option<String>,
    pub permissions: serde_json::Value,
    pub default_storno_daily_limit: f64,
    pub default_storno_emergency_limit: f64,
    pub can_approve_changes: bool,
    pub can_manage_users: bool,
    pub is_system_role: bool,
    pub audit_trail: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// POS user (SurrealDB document)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PosUser {
    pub id: i32,
    pub username: String,
    pub email: String,
    pub password_hash: String,
    pub full_name: String,
    pub role_id: i32,
    pub pos_device_id: Option<i32>,
    pub storno_daily_limit: f64,
    pub storno_emergency_limit: f64,
    pub storno_used_today: f64,
    pub trust_score: i32,
    pub is_active: bool,
    pub force_password_change: bool,
    pub last_login_at: Option<DateTime<Utc>>,
    pub last_login_ip: Option<String>,
    pub failed_login_attempts: i32,
    pub locked_until: Option<DateTime<Utc>>,
    pub user_preferences: Option<serde_json::Value>,
    pub audit_trail: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// User session (SurrealDB document)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UserSession {
    pub id: i32,
    pub session_id: String,
    pub user_id: i32,
    pub expires_at: DateTime<Utc>,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Menu category (SurrealDB document)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Category {
    pub id: i32,
    pub pos_device_id: i32,
    pub source_unique_identifier: String,
    pub category_names: serde_json::Value,
    pub category_type: String,
    pub parent_category_id: Option<i32>,
    pub default_linked_main_group_unique_identifier: Option<i32>,
    pub audit_trail: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Menu item (SurrealDB document)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MenuItem {
    pub id: i32,
    pub pos_device_id: i32,
    pub source_unique_identifier: String,
    pub associated_category_unique_identifier: i32,
    pub display_names: serde_json::Value,
    pub item_price_value: f64,
    pub pricing_schedules: Option<serde_json::Value>,
    pub availability_schedule: Option<serde_json::Value>,
    pub additional_item_attributes: Option<serde_json::Value>,
    pub item_flags: serde_json::Value,
    pub audit_trail: serde_json::Value,
    pub menu_item_number: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Active POS transaction (SurrealDB document)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActiveTransaction {
    pub id: i32,
    pub uuid: Uuid,
    pub status: String,
    pub user_id: Option<i32>,
    pub total_amount: f64,
    pub tax_amount: f64,
    pub business_date: String,
    pub metadata: Option<serde_json::Value>,
    pub resolution_status: String,
    pub payment_type: Option<String>,
    pub payment_amount: Option<f64>,
    pub bon_start: Option<DateTime<Utc>>,
    pub bon_end: Option<DateTime<Utc>>,
    pub bon_nr: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Transaction line item (SurrealDB document)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActiveTransactionItem {
    pub id: i32,
    pub active_transaction_id: i32,
    pub item_id: i32,
    pub quantity: f64,
    pub unit_price: f64,
    pub total_price: f64,
    pub tax_rate: f64,
    pub tax_amount: f64,
    pub notes: Option<String>,
    pub parent_transaction_item_id: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// === DSFinV-K Master Data ===

/// DSFinV-K location (Stamm_Orte)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, surrealdb::types::SurrealValue)]
pub struct DsfinvkLocation {
    pub id: Uuid,
    pub loc_name: String,
    pub loc_strasse: String,
    pub loc_plz: String,
    pub loc_ort: String,
    pub loc_land: String,
    pub loc_ustid: String,
}

/// DSFinV-K VAT mapping (Stamm_USt)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, surrealdb::types::SurrealValue)]
pub struct DsfinvkVatMapping {
    pub id: Uuid,
    pub internal_tax_rate: f64,
    pub dsfinvk_ust_schluessel: i32,
    pub description: String,
}

/// DSFinV-K TSE metadata (Stamm_TSE)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, surrealdb::types::SurrealValue)]
pub struct DsfinvkTse {
    pub id: Uuid,
    pub tse_id: String,
    pub tse_serial: String,
    pub tse_sig_algo: String,
    pub tse_zeitformat: String,
    pub tse_pd_encoding: String,
    pub tse_public_key: String,
    pub tse_zertifikat_i: String,
    pub tse_zertifikat_ii: String,
}

/// Sync outbox for POS (SurrealDB document)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SyncOutbox {
    pub id: Uuid,
    pub entity_type: String,
    pub entity_id: String,
    pub payload: serde_json::Value,
    pub error_count: i32,
    pub next_attempt_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}
