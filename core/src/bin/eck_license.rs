//! `eck-license` — 9eck product license minting tool (billing STUB).
//!
//! There is no billing backend yet; this CLI is how an operator generates the
//! issuer keypair and mints license tokens by hand. Later a 9eck.com billing
//! endpoint replaces the `issue` path — the token format and `licensing::verify`
//! on the relays stay the same.
//!
//! Usage:
//!   eck-license keygen
//!       → prints a fresh Ed25519 issuer keypair (PRIVKEY secret, PUBKEY public).
//!
//!   eck-license issue --priv <b64> --tenant <name> --mesh <mesh_id> \
//!                     [--tier paid] [--days 365] [--scope relay:payload ...]
//!       → prints a signed license token (set as ECK_LICENSE_TOKEN on the node).
//!
//!   eck-license verify --pub <b64> --token <token>
//!       → verifies offline and prints the decoded claims.

use base64::{engine::general_purpose::STANDARD, Engine};
use eck_core::licensing::{issue, verify, LicenseClaims, DEFAULT_GRACE_SECS, SCOPE_RELAY_PAYLOAD};
use eck_core::xelixir::admin_cert::{
    issue as cert_issue, verify as cert_verify, AdminCert, DEFAULT_GRACE_SECS as CERT_GRACE,
};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("");
    let rest = &args[args.len().min(2)..];
    let code = match cmd {
        "keygen" => keygen(),
        "issue" => issue_cmd(rest),
        "verify" => verify_cmd(rest),
        "fleet-root" => fleet_root_cmd(),
        "admin-cert" => admin_cert_cmd(rest),
        "cert-verify" => cert_verify_cmd(rest),
        _ => {
            eprintln!("{}", USAGE);
            2
        }
    };
    std::process::exit(code);
}

const USAGE: &str = "eck-license — 9eck license + fleet-admin minting\n\
  # product licensing (who paid)\n\
  keygen\n\
  issue --priv <b64> --tenant <name> --mesh <mesh_id> [--tier paid] [--days 365] [--scope relay:payload ...]\n\
  verify --pub <b64> --token <token>\n\
  # fleet-admin CA (who may run privileged ops) — SEPARATE root\n\
  fleet-root\n\
  admin-cert --root-priv <b64> [--admin-priv <b64>] [--label <s>] [--days 30] [--scope ops.<verb> ...]\n\
  cert-verify --root-pub <b64> --cert <token>";

fn keygen() -> i32 {
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;
    let sk = SigningKey::generate(&mut OsRng);
    let priv_b64 = STANDARD.encode(sk.to_bytes());
    let pub_b64 = STANDARD.encode(sk.verifying_key().to_bytes());
    println!("# 9eck license issuer keypair (Ed25519)");
    println!("# PRIVKEY — keep secret, only on the 9eck.com licensing authority:");
    println!("ECK_LICENSE_PRIVKEY={priv_b64}");
    println!("# PUBKEY — put on every paid relay (eck1/eck2/eck3) to verify offline:");
    println!("ECK_LICENSE_PUBKEY={pub_b64}");
    0
}

fn issue_cmd(args: &[String]) -> i32 {
    let priv_b64 = match flag(args, "--priv") {
        Some(v) => v,
        None => return missing("--priv"),
    };
    let tenant = match flag(args, "--tenant") {
        Some(v) => v,
        None => return missing("--tenant"),
    };
    let mesh = match flag(args, "--mesh") {
        Some(v) => v,
        None => return missing("--mesh"),
    };
    let tier = flag(args, "--tier").unwrap_or_else(|| "paid".to_string());
    let days: i64 = flag(args, "--days")
        .and_then(|d| d.parse().ok())
        .unwrap_or(365);
    let mut scopes = flags(args, "--scope");
    if scopes.is_empty() {
        scopes.push(SCOPE_RELAY_PAYLOAD.to_string());
    }

    let now = chrono::Utc::now().timestamp();
    let claims = LicenseClaims {
        tenant,
        tier,
        sub: mesh,
        scopes,
        iat: now,
        exp: now + days * 24 * 3600,
    };

    match issue(&priv_b64, &claims) {
        Ok(token) => {
            eprintln!(
                "# license: tenant={} tier={} mesh={} scopes={:?} valid {} days",
                claims.tenant, claims.tier, claims.sub, claims.scopes, days
            );
            println!("ECK_LICENSE_TOKEN={token}");
            0
        }
        Err(e) => {
            eprintln!("issue failed: {e}");
            1
        }
    }
}

fn verify_cmd(args: &[String]) -> i32 {
    let pub_b64 = match flag(args, "--pub") {
        Some(v) => v,
        None => return missing("--pub"),
    };
    let token = match flag(args, "--token") {
        Some(v) => v,
        None => return missing("--token"),
    };
    let now = chrono::Utc::now().timestamp();
    match verify(&pub_b64, &token, now, DEFAULT_GRACE_SECS) {
        Ok(c) => {
            println!("{}", serde_json::to_string_pretty(&c).unwrap_or_default());
            println!("# is_paid={}  expires_in_days={}", c.is_paid(), (c.exp - now) / 86400);
            0
        }
        Err(e) => {
            eprintln!("INVALID: {e}");
            1
        }
    }
}

// ── fleet-admin CA (separate root from product licensing) ──────────────────

fn fleet_root_cmd() -> i32 {
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;
    let sk = SigningKey::generate(&mut OsRng);
    let priv_b64 = STANDARD.encode(sk.to_bytes());
    let pub_b64 = STANDARD.encode(sk.verifying_key().to_bytes());
    println!("# 9eck FLEET-ADMIN root keypair (Ed25519) — SEPARATE from the license root.");
    println!("# PRIVKEY — keep OFFLINE; only ever runs `admin-cert` to mint operational certs:");
    println!("ECK_FLEET_ROOT_PRIVKEY={priv_b64}");
    println!("# PUBKEY — bake into EVERY fleet node's env (provisioning) as the trust anchor:");
    println!("ECK_FLEET_ROOT_PUBKEY={pub_b64}");
    0
}

fn admin_cert_cmd(args: &[String]) -> i32 {
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;
    let root_priv = match flag(args, "--root-priv") {
        Some(v) => v,
        None => return missing("--root-priv"),
    };
    let label = flag(args, "--label").unwrap_or_else(|| "fleet-control".to_string());
    let days: i64 = flag(args, "--days").and_then(|d| d.parse().ok()).unwrap_or(30);
    let scopes = flags(args, "--scope");

    // Use a provided operational admin key, or generate a fresh one.
    let (admin_priv, admin_pub, generated) = match flag(args, "--admin-priv") {
        Some(p) => {
            let bytes = match STANDARD.decode(p.trim()) {
                Ok(b) if b.len() == 32 => b,
                _ => {
                    eprintln!("--admin-priv must be a 32-byte STANDARD base64 seed");
                    return 2;
                }
            };
            let sk = SigningKey::from_bytes(&bytes.as_slice().try_into().unwrap());
            (p, STANDARD.encode(sk.verifying_key().to_bytes()), false)
        }
        None => {
            let sk = SigningKey::generate(&mut OsRng);
            (
                STANDARD.encode(sk.to_bytes()),
                STANDARD.encode(sk.verifying_key().to_bytes()),
                true,
            )
        }
    };

    let now = chrono::Utc::now().timestamp();
    let cert = AdminCert {
        pubkey: admin_pub.clone(),
        label: label.clone(),
        scopes: scopes.clone(),
        iat: now,
        exp: now + days * 24 * 3600,
    };
    match cert_issue(&root_priv, &cert) {
        Ok(token) => {
            eprintln!(
                "# admin cert: label={} scopes={:?} valid {} days (pubkey={})",
                label,
                if scopes.is_empty() { vec!["*".to_string()] } else { scopes },
                days,
                admin_pub
            );
            eprintln!("# Put these THREE on the control node (e.g. 222.64):");
            if generated {
                println!("ECK_FLEET_ADMIN_PRIVKEY={admin_priv}");
            } else {
                println!("# (reusing supplied --admin-priv)");
            }
            println!("ECK_FLEET_ADMIN_PUBKEY={admin_pub}");
            println!("ECK_FLEET_ADMIN_CERT={token}");
            0
        }
        Err(e) => {
            eprintln!("admin-cert issue failed: {e}");
            1
        }
    }
}

fn cert_verify_cmd(args: &[String]) -> i32 {
    let root_pub = match flag(args, "--root-pub") {
        Some(v) => v,
        None => return missing("--root-pub"),
    };
    let token = match flag(args, "--cert") {
        Some(v) => v,
        None => return missing("--cert"),
    };
    let now = chrono::Utc::now().timestamp();
    match cert_verify(&root_pub, &token, now, CERT_GRACE) {
        Ok(c) => {
            println!("{}", serde_json::to_string_pretty(&c).unwrap_or_default());
            println!("# expires_in_days={}", (c.exp - now) / 86400);
            0
        }
        Err(e) => {
            eprintln!("INVALID: {e}");
            1
        }
    }
}

/// First value following `name`.
fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// All values following each occurrence of `name` (repeatable flag).
fn flags(args: &[String], name: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (i, a) in args.iter().enumerate() {
        if a == name {
            if let Some(v) = args.get(i + 1) {
                out.push(v.clone());
            }
        }
    }
    out
}

fn missing(name: &str) -> i32 {
    eprintln!("missing required {name}\n{USAGE}");
    2
}
