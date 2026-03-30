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
    #[serde(rename = "createdAt")]
    pub created_at: DateTime<Utc>,
    #[serde(rename = "updatedAt")]
    pub updated_at: DateTime<Utc>,
    #[serde(skip_serializing)]
    pub deleted_at: Option<DateTime<Utc>>,
}
