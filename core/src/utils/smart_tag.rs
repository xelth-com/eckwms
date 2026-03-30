use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use uuid::Uuid;

/// SmartTag V2 — compact 19-byte binary barcode format.
///
/// Layout:
/// - Byte 0:    `tag_type` as ASCII `u8` ('i' = Item, 'b' = Box, 'p' = Place, 'l' = Label)
/// - Bytes 1-2: `flags` as big-endian `u16` (0x0002 = V2)
/// - Bytes 3-18: `id` as 16-byte UUID
#[derive(Clone, Debug, PartialEq)]
pub struct SmartTag {
    pub tag_type: char,
    pub flags: u16,
    pub id: Uuid,
}

impl SmartTag {
    /// Create a new V2 SmartTag.
    pub fn new(tag_type: char, id: Uuid) -> Self {
        Self {
            tag_type,
            flags: 0x0002,
            id,
        }
    }

    /// Encode to URL-safe Base64 (no padding). Returns a 26-char string.
    pub fn encode(&self) -> String {
        let mut buf = [0u8; 19];
        buf[0] = self.tag_type as u8;
        buf[1..3].copy_from_slice(&self.flags.to_be_bytes());
        buf[3..19].copy_from_slice(self.id.as_bytes());
        URL_SAFE_NO_PAD.encode(buf)
    }

    /// Decode from URL-safe Base64.
    pub fn decode(encoded: &str) -> anyhow::Result<Self> {
        let bytes = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|e| anyhow::anyhow!("invalid base64: {}", e))?;

        if bytes.len() != 19 {
            anyhow::bail!(
                "invalid SmartTag length: expected 19 bytes, got {}",
                bytes.len()
            );
        }

        let tag_type = bytes[0] as char;
        let flags = u16::from_be_bytes([bytes[1], bytes[2]]);
        let id = Uuid::from_bytes(
            bytes[3..19]
                .try_into()
                .map_err(|_| anyhow::anyhow!("invalid UUID bytes"))?,
        );

        Ok(Self {
            tag_type,
            flags,
            id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let id = Uuid::new_v4();
        let tag = SmartTag::new('i', id);
        let encoded = tag.encode();
        let decoded = SmartTag::decode(&encoded).unwrap();
        assert_eq!(tag, decoded);
        assert_eq!(decoded.flags, 0x0002);
        assert_eq!(decoded.tag_type, 'i');
        assert_eq!(decoded.id, id);
    }

    #[test]
    fn all_types() {
        let id = Uuid::new_v4();
        for t in ['i', 'b', 'p', 'l'] {
            let tag = SmartTag::new(t, id);
            let decoded = SmartTag::decode(&tag.encode()).unwrap();
            assert_eq!(decoded.tag_type, t);
        }
    }

    #[test]
    fn invalid_length() {
        assert!(SmartTag::decode("AAAA").is_err());
    }
}
