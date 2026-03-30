use sha2::{Sha256, Digest};

/// Compute a 64-bit SimHash from character bigrams, keyed with SYNC_SECRET as pepper.
/// Similar strings produce hashes with low Hamming distance,
/// enabling privacy-preserving record linkage (PPRL).
/// The pepper prevents dictionary attacks on the hash tokens.
fn simhash(text: &str) -> u64 {
    let pepper = std::env::var("SYNC_SECRET").unwrap_or_else(|_| "eck_default_pepper".to_string());
    let lower = text.to_lowercase();
    let chars: Vec<char> = lower.chars().collect();

    if chars.len() < 2 {
        // Too short for bigrams — keyed SHA256 hash directly
        let mut hasher = Sha256::new();
        hasher.update(lower.as_bytes());
        hasher.update(pepper.as_bytes());
        let digest = hasher.finalize();
        return u64::from_be_bytes(digest[0..8].try_into().unwrap());
    }

    let mut v = [0i32; 64];

    for window in chars.windows(2) {
        let bigram: String = window.iter().collect();
        let mut hasher = Sha256::new();
        hasher.update(bigram.as_bytes());
        hasher.update(pepper.as_bytes());
        let digest = hasher.finalize();
        let h = u64::from_be_bytes(digest[0..8].try_into().unwrap());

        for i in 0..64 {
            if (h >> i) & 1 == 1 {
                v[i] += 1;
            } else {
                v[i] -= 1;
            }
        }
    }

    let mut result: u64 = 0;
    for i in 0..64 {
        if v[i] > 0 {
            result |= 1u64 << i;
        }
    }
    result
}

/// Obfuscate a PII string using keyed SimHash.
/// Returns a token like `Name_8E5F3A1B00000000` that preserves similarity
/// (similar names → similar hashes) without revealing the original value.
pub fn obfuscate_pii(text: &str, pii_type: &str) -> String {
    format!("{}_{:016X}", pii_type, simhash(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() {
        std::env::set_var("SYNC_SECRET", "test_secret");
    }

    #[test]
    fn similar_strings_have_low_hamming_distance() {
        setup();
        let h1 = simhash("Hans Müller Berlin");
        let h2 = simhash("Hans Mueller Berlin");
        let distance = (h1 ^ h2).count_ones();
        assert!(distance < 25, "Hamming distance too high: {distance}");
    }

    #[test]
    fn different_strings_have_high_hamming_distance() {
        setup();
        let h1 = simhash("Hans Müller");
        let h2 = simhash("InBody 770 Reparatur");
        let distance = (h1 ^ h2).count_ones();
        assert!(distance > 10, "Hamming distance too low for dissimilar strings: {distance}");
    }

    #[test]
    fn obfuscate_format() {
        setup();
        let token = obfuscate_pii("Ivan Petrov", "Name");
        assert!(token.starts_with("Name_"));
        assert_eq!(token.len(), 5 + 16); // "Name_" + 16 hex chars
    }

    #[test]
    fn deterministic_with_same_pepper() {
        setup();
        let h1 = simhash("Test Person");
        let h2 = simhash("Test Person");
        assert_eq!(h1, h2, "Same input + same pepper must produce same hash");
    }
}
