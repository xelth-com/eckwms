use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use surrealdb::types::SurrealValue;
use uuid::Uuid;

/// User authentication entity (SurrealDB document)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, surrealdb::types::SurrealValue)]
pub struct User {
    pub id: Uuid,
    pub username: String,
    #[serde(skip_serializing)]
    pub password: String,
    pub email: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub role: String,
    #[serde(rename = "userType")]
    pub user_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub company: Option<String>,
    #[serde(rename = "googleId", skip_serializing_if = "Option::is_none")]
    pub google_id: Option<String>,
    #[serde(skip_serializing)]
    pub pin: String,
    #[serde(rename = "isActive")]
    pub is_active: bool,
    #[serde(rename = "lastLogin", skip_serializing_if = "Option::is_none")]
    pub last_login: Option<DateTime<Utc>>,
    #[serde(skip_serializing)]
    pub failed_login_attempts: i64,
    #[serde(rename = "preferredLanguage")]
    pub preferred_language: String,
    /// Additional languages this user speaks (BCP-47-ish codes, e.g. ["de","ko"]).
    /// Optional so existing rows without the field still deserialize.
    #[serde(rename = "languages", default, skip_serializing_if = "Option::is_none")]
    pub languages: Option<Vec<String>>,
    /// Force a password change on next login (set for bulk-seeded accounts that
    /// share a generated password). Optional so existing rows deserialize.
    #[serde(rename = "mustChangePassword", default, skip_serializing_if = "Option::is_none")]
    pub must_change_password: Option<bool>,
    #[serde(rename = "createdAt")]
    pub created_at: DateTime<Utc>,
    #[serde(rename = "updatedAt")]
    pub updated_at: DateTime<Utc>,
    #[serde(skip_serializing)]
    pub deleted_at: Option<DateTime<Utc>>,
}
