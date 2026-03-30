use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// OdooString handles Odoo's dynamic typing where empty text fields are returned as boolean `false`.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct OdooString(pub String);

impl<'de> Deserialize<'de> for OdooString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum StringOrBool {
            String(String),
            Bool(bool),
        }

        match StringOrBool::deserialize(deserializer)? {
            StringOrBool::String(s) => Ok(OdooString(s)),
            StringOrBool::Bool(b) => {
                if !b {
                    Ok(OdooString(String::new()))
                } else {
                    Ok(OdooString("true".to_string()))
                }
            }
        }
    }
}

impl Serialize for OdooString {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl fmt::Display for OdooString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
