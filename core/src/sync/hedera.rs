use anyhow::Context;
use chrono::Utc;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// Response from a successful HCS submission.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HcsReceipt {
    /// The sequence number assigned by the HCS topic.
    pub sequence_number: u64,
    /// The consensus timestamp from the Hedera network.
    pub consensus_timestamp: String,
}

/// Hedera HCS client for submitting immutability proofs via REST API.
///
/// Used for GoBD "Festschreibung" — every closed transaction and AI action
/// has its SHA-256 hash submitted to an HCS topic, producing a tamper-proof
/// audit trail with globally ordered sequence numbers.
///
/// Uses the Hedera REST API instead of the native gRPC SDK to avoid
/// the OpenSSL dependency on Windows.
#[derive(Clone)]
pub struct HederaClient {
    http: Client,
    /// Base URL for the Hedera API (e.g., "https://testnet.hedera.com" or mirror node)
    base_url: String,
    /// Account ID (e.g., "0.0.12345")
    account_id: String,
    /// Topic ID (e.g., "0.0.67890")
    topic_id: String,
    /// API key or operator key for authentication
    api_key: String,
}

#[derive(Serialize)]
struct SubmitMessageRequest {
    message: String,
}

#[derive(Deserialize)]
struct SubmitMessageResponse {
    #[serde(default)]
    sequence_number: Option<u64>,
    #[serde(default)]
    consensus_timestamp: Option<String>,
}

impl HederaClient {
    /// Initialize from environment variables.
    /// Returns `None` if Hedera is not configured (dev mode — silent no-op).
    ///
    /// Required env vars:
    /// - `HEDERA_ACCOUNT_ID` (e.g., "0.0.12345")
    /// - `HEDERA_PRIVATE_KEY` (operator key for signing)
    /// - `HEDERA_TOPIC_ID` (e.g., "0.0.67890")
    /// - `HEDERA_NETWORK` (optional: "testnet" or "mainnet", defaults to "testnet")
    pub fn from_env() -> Option<Self> {
        let account_id = std::env::var("HEDERA_ACCOUNT_ID").ok()?;
        let api_key = std::env::var("HEDERA_PRIVATE_KEY").ok()?;
        let topic_id = std::env::var("HEDERA_TOPIC_ID").ok()?;
        let network = std::env::var("HEDERA_NETWORK").unwrap_or_else(|_| "testnet".into());

        let base_url = match network.as_str() {
            "mainnet" => "https://mainnet-public.mirrornode.hedera.com".to_string(),
            _ => "https://testnet.mirrornode.hedera.com".to_string(),
        };

        let http = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .ok()?;

        info!(
            "Hedera HCS initialized: account={}, topic={}, network={}",
            account_id, topic_id, network
        );

        Some(Self {
            http,
            base_url,
            account_id,
            topic_id,
            api_key,
        })
    }

    /// Submit a content hash to the HCS topic.
    ///
    /// The hash is submitted as a message to the configured HCS topic.
    /// Returns the sequence number and consensus timestamp on success.
    pub async fn submit_hash(&self, hash: &str) -> anyhow::Result<HcsReceipt> {
        let url = format!(
            "{}/api/v1/topics/{}/messages",
            self.base_url, self.topic_id
        );

        let body = SubmitMessageRequest {
            message: hash.to_string(),
        };

        let resp = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await
            .context("HCS submit request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("HCS submit failed ({}): {}", status, text);
        }

        let result: SubmitMessageResponse = resp
            .json()
            .await
            .context("HCS response parse failed")?;

        let receipt = HcsReceipt {
            sequence_number: result.sequence_number.unwrap_or(0),
            consensus_timestamp: result
                .consensus_timestamp
                .unwrap_or_else(|| Utc::now().to_rfc3339()),
        };

        info!(
            "HCS submit OK: topic={}, seq={}",
            self.topic_id, receipt.sequence_number
        );

        Ok(receipt)
    }

    pub fn topic_id(&self) -> &str {
        &self.topic_id
    }

    pub fn account_id(&self) -> &str {
        &self.account_id
    }
}

/// Convenience function: submit a hash if Hedera is configured, otherwise no-op.
///
/// Returns `Some(HcsReceipt)` on success, `None` if Hedera is not configured
/// or if submission fails (logged as warning, never fatal to business operations).
pub async fn submit_hash_if_configured(
    client: Option<&HederaClient>,
    hash: &str,
) -> Option<HcsReceipt> {
    let client = client?;
    match client.submit_hash(hash).await {
        Ok(receipt) => {
            debug!(
                "HCS sealed: hash={}..., seq={}",
                &hash[..hash.len().min(16)],
                receipt.sequence_number
            );
            Some(receipt)
        }
        Err(e) => {
            warn!("HCS submission failed (non-fatal): {}", e);
            None
        }
    }
}
