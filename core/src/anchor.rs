//! Pure-Rust Hedera HCS submitter — **NO openssl** (tonic + rustls + prost).
//!
//! We hand-build Hedera transactions against `hedera-proto` and submit them
//! over gRPC, signing the body with our existing Ed25519 stack. This is the
//! *real* anchor path: the legacy `sync::hedera` reqwest client POSTs to a
//! **mirror node**, which is read-only and cannot write to a topic.
//!
//! Ported from `xelixir/crates/server/src/anchor.rs` (proven on testnet topics
//! 0.0.9158506 / 0.0.9158698). Keep `cargo tree -i openssl-sys` empty.
//!
//! Config (env):
//! - `HEDERA_ACCOUNT_ID`  — operator/payer, `0.0.N`.
//! - `HEDERA_KEY`         — operator Ed25519 **private** key, 32 bytes hex.
//! - `HEDERA_TOPIC_ID`    — the audit anchor topic, `0.0.N`.
//! - `HEDERA_NODE_URL`    — gRPC node endpoint (default a testnet node).
//! - `HEDERA_NODE_ACCOUNT`— node account `0.0.N` (default `0.0.3`).
//!
//! **Never commit `HEDERA_KEY`.** It lives only in a systemd drop-in (mode 600).

use ed25519_dalek::{Signer, SigningKey};
use hedera_proto::services;
use hedera_proto::services::consensus_service_client::ConsensusServiceClient;
use prost::Message;

fn now_ts() -> services::Timestamp {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    services::Timestamp {
        seconds: d.as_secs() as i64,
        nanos: d.subsec_nanos() as i32,
    }
}

/// `0.0.N` → `AccountId{shard:0, realm:0, accountNum:N}`.
fn acct(num: i64) -> services::AccountId {
    services::AccountId {
        shard_num: 0,
        realm_num: 0,
        account: Some(services::account_id::Account::AccountNum(num)),
    }
}

/// Parse the trailing entity number from `"0.0.N"`.
pub fn entity_num(s: &str) -> Option<i64> {
    s.trim().rsplit('.').next()?.parse().ok()
}

fn signing_key() -> Option<SigningKey> {
    let kh = std::env::var("HEDERA_KEY").ok()?;
    let bytes = hex::decode(kh.trim()).ok()?;
    let arr = <[u8; 32]>::try_from(bytes.as_slice()).ok()?;
    Some(SigningKey::from_bytes(&arr))
}

/// Read-only mirror-node REST base URL for the configured network. Used to read
/// back consensus sequence numbers / timestamps after a submit (the node submit
/// itself returns only a precheck + tx id). Override with `HEDERA_MIRROR_URL`.
pub fn mirror_base_url() -> String {
    if let Ok(u) = std::env::var("HEDERA_MIRROR_URL") {
        let u = u.trim().trim_end_matches('/');
        if !u.is_empty() {
            return u.to_string();
        }
    }
    match std::env::var("HEDERA_NETWORK").as_deref() {
        Ok("mainnet") => "https://mainnet.mirrornode.hedera.com".into(),
        _ => "https://testnet.mirrornode.hedera.com".into(),
    }
}

/// True when the minimum env for a real HCS submission is present.
pub fn is_configured() -> bool {
    std::env::var("HEDERA_ACCOUNT_ID").is_ok()
        && std::env::var("HEDERA_KEY").is_ok()
        && std::env::var("HEDERA_TOPIC_ID").is_ok()
}

/// Build, sign and submit one transaction carrying `data`. Returns
/// `(node_precheck_code, mirror_tx_id)`; precheck `0` = the node accepted it.
async fn submit_body(data: services::transaction_body::Data) -> Result<(i32, String), String> {
    let op_num = entity_num(
        &std::env::var("HEDERA_ACCOUNT_ID").map_err(|_| "HEDERA_ACCOUNT_ID unset")?,
    )
    .ok_or("bad HEDERA_ACCOUNT_ID")?;
    let sk = signing_key().ok_or("HEDERA_KEY missing/invalid")?;
    let pubkey = sk.verifying_key().to_bytes().to_vec();
    let node_num: i64 = std::env::var("HEDERA_NODE_ACCOUNT")
        .ok()
        .and_then(|s| entity_num(&s))
        .unwrap_or(3);
    let node_url =
        std::env::var("HEDERA_NODE_URL").unwrap_or_else(|_| "http://34.94.106.61:50211".into());

    let valid_start = now_ts();
    let tx_id = services::TransactionId {
        transaction_valid_start: Some(valid_start.clone()),
        account_id: Some(acct(op_num)),
        scheduled: false,
        nonce: 0,
    };
    let body = services::TransactionBody {
        transaction_id: Some(tx_id),
        node_account_id: Some(acct(node_num)),
        transaction_fee: 200_000_000, // 2 ℏ max fee
        transaction_valid_duration: Some(services::Duration { seconds: 120 }),
        memo: "9eck-audit-anchor".into(),
        data: Some(data),
        ..Default::default()
    };
    let body_bytes = body.encode_to_vec();
    let sig = sk.sign(&body_bytes).to_bytes().to_vec();
    let signed = services::SignedTransaction {
        body_bytes: body_bytes.clone(),
        sig_map: Some(services::SignatureMap {
            sig_pair: vec![services::SignaturePair {
                pub_key_prefix: pubkey,
                signature: Some(services::signature_pair::Signature::Ed25519(sig)),
            }],
        }),
        ..Default::default()
    };
    let tx = services::Transaction {
        signed_transaction_bytes: signed.encode_to_vec(),
        ..Default::default()
    };

    let mut client = ConsensusServiceClient::connect(node_url.clone())
        .await
        .map_err(|e| format!("connect {node_url}: {e}"))?;
    let resp = client
        .submit_message(tx)
        .await
        .map_err(|e| format!("submit rpc: {e}"))?;
    let precheck = resp.into_inner().node_transaction_precheck_code;

    // Mirror-node transaction id format: accountNum-seconds-nanos.
    let mirror_txid = format!("0.0.{}-{}-{}", op_num, valid_start.seconds, valid_start.nanos);
    Ok((precheck, mirror_txid))
}

/// Create the anchor HCS topic with our Ed25519 key as BOTH admin and submit
/// key — only we can anchor to it (and manage it). Auto-renew ~91 d. The new
/// topic id is read from the mirror node afterwards (entity_id of the tx).
pub async fn create_topic() -> Result<(i32, String), String> {
    let pubkey = signing_key()
        .ok_or("HEDERA_KEY missing/invalid")?
        .verifying_key()
        .to_bytes()
        .to_vec();
    let key = services::Key {
        key: Some(services::key::Key::Ed25519(pubkey)),
    };
    submit_body(services::transaction_body::Data::ConsensusCreateTopic(
        services::ConsensusCreateTopicTransactionBody {
            memo: "9eck-audit-anchor".into(),
            admin_key: Some(key.clone()),
            submit_key: Some(key),
            auto_renew_period: Some(services::Duration {
                seconds: 7_890_000,
            }),
            ..Default::default()
        },
    ))
    .await
}

/// Submit one message to HCS `topic_num`.
pub async fn submit_message(topic_num: i64, message: &[u8]) -> Result<(i32, String), String> {
    submit_body(services::transaction_body::Data::ConsensusSubmitMessage(
        services::ConsensusSubmitMessageTransactionBody {
            topic_id: Some(services::TopicId {
                shard_num: 0,
                realm_num: 0,
                topic_num,
            }),
            message: message.to_vec(),
            ..Default::default()
        },
    ))
    .await
}
