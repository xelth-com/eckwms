//! Byte-wise Shamir secret sharing over GF(256) (the AES field, reduction
//! polynomial `0x11b`). Used to split the KEK-wrapped vault blob into `n` shares,
//! one per eck safe server, recoverable from any `k` of them (`.eck/PII_VAULT.md`
//! §2: `shares = Shamir(blob, k=2, n=3)`).
//!
//! A single share is information-theoretically independent of the secret: with
//! `k = 2`, one share is a point on a random line and reveals nothing. The eck
//! servers store opaque share bytes they cannot interpret.

use rand::RngCore;
use zeroize::Zeroizing;

/// One Shamir share: the evaluation point `index` (x, in `1..=n`) and one output
/// byte per secret byte (`data.len() == secret.len()`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Share {
    pub index: u8,
    pub data: Vec<u8>,
}

// ─── GF(256) arithmetic (AES field, poly 0x11b) ──────────────────────────────

/// Carry-less multiply in GF(2^8) with reduction by `x^8 + x^4 + x^3 + x + 1`.
#[inline]
fn gf_mul(mut a: u8, mut b: u8) -> u8 {
    let mut p = 0u8;
    for _ in 0..8 {
        if b & 1 != 0 {
            p ^= a;
        }
        let hi = a & 0x80;
        a <<= 1;
        if hi != 0 {
            a ^= 0x1b; // reduce (0x11b truncated to 8 bits)
        }
        b >>= 1;
    }
    p
}

/// `a^n` in GF(2^8).
#[inline]
fn gf_pow(mut a: u8, mut n: u32) -> u8 {
    let mut r = 1u8;
    while n > 0 {
        if n & 1 == 1 {
            r = gf_mul(r, a);
        }
        a = gf_mul(a, a);
        n >>= 1;
    }
    r
}

/// Multiplicative inverse in GF(2^8): `a^(2^8 - 2) = a^254`. `inv(0) == 0`.
#[inline]
fn gf_inv(a: u8) -> u8 {
    gf_pow(a, 254)
}

// ─── split / combine ─────────────────────────────────────────────────────────

/// Split `secret` into `n` shares recoverable from any `k`. Polynomial degree is
/// `k - 1`; each byte gets an independent random polynomial with the secret byte
/// as the constant term, evaluated at `x = 1..=n`.
pub fn split(secret: &[u8], k: u8, n: u8, rng: &mut impl RngCore) -> Vec<Share> {
    debug_assert!(k >= 1 && n >= k, "need 1 <= k <= n");
    let mut shares: Vec<Share> = (1..=n)
        .map(|i| Share {
            index: i,
            data: Vec::with_capacity(secret.len()),
        })
        .collect();

    let mut coeffs = vec![0u8; k as usize];
    for &byte in secret {
        // coeffs[0] = the secret byte; coeffs[1..] fresh random each byte.
        coeffs[0] = byte;
        if k > 1 {
            rng.fill_bytes(&mut coeffs[1..]);
        }
        for s in shares.iter_mut() {
            // Horner evaluation of the polynomial at x = s.index.
            let x = s.index;
            let mut acc = 0u8;
            for &c in coeffs.iter().rev() {
                acc = gf_mul(acc, x) ^ c;
            }
            s.data.push(acc);
        }
    }
    // Wipe the transient coefficient buffer (held random poly coeffs).
    let _ = Zeroizing::new(std::mem::take(&mut coeffs));
    shares
}

/// Reconstruct the secret from `shares` via Lagrange interpolation at `x = 0`.
/// Needs `>= 2` shares with distinct non-zero indexes; `secret_len` is the
/// original byte length (equal to any share's `data.len()`). A tampered share
/// yields garbage here — the blob's AEAD tag is what catches it downstream.
pub fn combine(shares: &[Share], secret_len: usize) -> Result<Zeroizing<Vec<u8>>, String> {
    if shares.len() < 2 {
        return Err(format!("need >= 2 shares, got {}", shares.len()));
    }
    for (i, s) in shares.iter().enumerate() {
        if s.index == 0 {
            return Err("share index 0 is invalid".into());
        }
        if shares[..i].iter().any(|o| o.index == s.index) {
            return Err(format!("duplicate share index {}", s.index));
        }
        if s.data.len() < secret_len {
            return Err(format!(
                "share {} too short: {} < {secret_len}",
                s.index,
                s.data.len()
            ));
        }
    }

    let mut out = Zeroizing::new(vec![0u8; secret_len]);
    for (pos, slot) in out.iter_mut().enumerate() {
        let mut secret_byte = 0u8;
        for (i, si) in shares.iter().enumerate() {
            let xi = si.index;
            let yi = si.data[pos];
            // Lagrange basis at 0: L_i(0) = prod_{j!=i} (0 - xj)/(xi - xj).
            // In GF(2^8), subtraction is XOR and (0 - xj) == xj.
            let mut num = 1u8;
            let mut den = 1u8;
            for (j, sj) in shares.iter().enumerate() {
                if i == j {
                    continue;
                }
                let xj = sj.index;
                num = gf_mul(num, xj);
                den = gf_mul(den, xi ^ xj);
            }
            let li = gf_mul(num, gf_inv(den));
            secret_byte ^= gf_mul(yi, li);
        }
        *slot = secret_byte;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{rngs::StdRng, SeedableRng};

    #[test]
    fn gf_inverse_is_consistent() {
        for a in 1u8..=255 {
            assert_eq!(gf_mul(a, gf_inv(a)), 1, "inv({a}) wrong");
        }
        assert_eq!(gf_inv(0), 0);
    }

    #[test]
    fn round_trip_every_pair_2_of_3() {
        let mut rng = StdRng::seed_from_u64(42);
        let secret = b"tenant-master-key-and-epochs-blob-1234567890".to_vec();
        let shares = split(&secret, 2, 3, &mut rng);
        assert_eq!(shares.len(), 3);
        for a in 0..3 {
            for b in 0..3 {
                if a == b {
                    continue;
                }
                let subset = [shares[a].clone(), shares[b].clone()];
                let got = combine(&subset, secret.len()).unwrap();
                assert_eq!(got.as_slice(), secret.as_slice(), "pair ({a},{b}) failed");
            }
        }
        // All three together also reconstruct.
        assert_eq!(combine(&shares, secret.len()).unwrap().as_slice(), secret.as_slice());
    }

    #[test]
    fn single_share_does_not_equal_secret() {
        let mut rng = StdRng::seed_from_u64(7);
        let secret = vec![0xAAu8; 64];
        let shares = split(&secret, 2, 3, &mut rng);
        // No share should be byte-identical to the secret (constant term is
        // masked by the random slope times a non-zero x).
        for s in &shares {
            assert_ne!(s.data, secret, "share {} leaked the secret", s.index);
        }
    }

    #[test]
    fn tampered_share_yields_garbage() {
        let mut rng = StdRng::seed_from_u64(9);
        let secret = b"0123456789abcdef0123456789abcdef".to_vec();
        let mut shares = split(&secret, 2, 3, &mut rng);
        shares[0].data[3] ^= 0xFF; // flip a byte
        let subset = [shares[0].clone(), shares[1].clone()];
        let got = combine(&subset, secret.len()).unwrap();
        assert_ne!(got.as_slice(), secret.as_slice(), "tamper went undetected");
    }

    #[test]
    fn combine_rejects_too_few_or_bad_indexes() {
        let mut rng = StdRng::seed_from_u64(1);
        let shares = split(b"x", 2, 3, &mut rng);
        assert!(combine(&shares[..1], 1).is_err());
        let dup = [shares[0].clone(), shares[0].clone()];
        assert!(combine(&dup, 1).is_err());
    }

    #[test]
    fn empty_secret_round_trips() {
        let mut rng = StdRng::seed_from_u64(3);
        let shares = split(&[], 2, 3, &mut rng);
        assert!(shares.iter().all(|s| s.data.is_empty()));
        let got = combine(&shares[..2], 0).unwrap();
        assert!(got.is_empty());
    }

    #[test]
    fn property_random_lengths() {
        let mut rng = StdRng::seed_from_u64(0xDEADBEEF);
        for len in 0..1000usize {
            let mut secret = vec![0u8; len];
            rng.fill_bytes(&mut secret);
            let shares = split(&secret, 2, 3, &mut rng);
            // Reconstruct from shares 1 and 3 (indexes 1 and 3).
            let subset = [shares[0].clone(), shares[2].clone()];
            let got = combine(&subset, len).unwrap();
            assert_eq!(got.as_slice(), secret.as_slice(), "len {len} failed");
        }
    }
}
