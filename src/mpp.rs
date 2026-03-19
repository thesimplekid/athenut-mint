//! MPP (Machine Payments Protocol) Lightning charge implementation.
//!
//! Implements the Lightning charge intent per the IETF spec:
//! https://paymentauth.org/draft-lightning-charge-00
//!
//! This module handles:
//! - Challenge generation (WWW-Authenticate: Payment header)
//! - Credential parsing (Authorization: Payment header)
//! - Preimage verification (sha256(preimage) == paymentHash)
//! - Receipt generation (Payment-Receipt header)
//! - Challenge state storage in the mint's KV store

use std::str::FromStr;
use std::sync::Arc;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use cdk::mint::Mint;
use cdk::wallet::Wallet;
use lightning_invoice::Bolt11Invoice;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::cdk_wallet::{cents_to_msats, get_usd_price};

const KV_PRIMARY_NAMESPACE: &str = "athenut";
const KV_SECONDARY_NAMESPACE: &str = "mpp_challenge";

// -- Protocol types --

/// The challenge parameters sent in the WWW-Authenticate header.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChallengeParams {
    pub id: String,
    pub realm: String,
    pub method: String,
    pub intent: String,
    pub request: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires: Option<String>,
}

/// The base64url-decoded request object within a challenge.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeRequest {
    pub amount: String,
    pub currency: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub method_details: LightningMethodDetails,
}

/// Lightning-specific method details within the challenge request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LightningMethodDetails {
    pub invoice: String,
    pub payment_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
}

/// The credential sent by the client in the Authorization header.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credential {
    pub challenge: ChallengeParams,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub payload: PreimagePayload,
}

/// The Lightning-specific credential payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreimagePayload {
    pub preimage: String,
}

/// The receipt sent in the Payment-Receipt header.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Receipt {
    pub method: String,
    pub challenge_id: String,
    pub reference: String,
    pub status: String,
    pub timestamp: String,
}

/// Persisted challenge state stored in the mint's KV store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChallengeState {
    /// The full challenge parameters (for verification against echoed challenge)
    pub challenge_params: ChallengeParams,
    /// The payment hash from the bolt11 invoice (hex)
    pub payment_hash: String,
    /// The amount in satoshis
    pub amount_sats: u64,
    /// Unix timestamp when the challenge expires
    pub expires_at: u64,
    /// Whether this challenge has been consumed
    pub consumed: bool,
    /// The original search query this challenge was issued for
    #[serde(default)]
    pub query: String,
}

/// Runtime state for MPP payment handling.
pub struct MppState {
    pub realm: String,
    pub wallet: Arc<Wallet>,
    pub cost_per_xsr_cents: u64,
}

// -- Challenge generation --

/// Generate a bolt11 invoice via the backing Cashu wallet and build an MPP challenge.
///
/// Returns the ChallengeState to be stored and the WWW-Authenticate header value.
pub async fn generate_challenge(
    mpp_state: &MppState,
    description: &str,
    query: &str,
) -> anyhow::Result<(ChallengeState, String)> {
    // Calculate cost in sats using live BTC price
    let usd_price = get_usd_price().await.map_err(|e| anyhow::anyhow!(e))?;
    let msats = cents_to_msats(mpp_state.cost_per_xsr_cents, usd_price);

    // Convert msats to sats (round up)
    let amount_sats = msats.div_ceil(1000);

    // Create a mint quote on the backing wallet to get a bolt11 invoice
    let quote = mpp_state
        .wallet
        .mint_quote(
            cdk::nuts::PaymentMethod::BOLT11,
            Some(cdk::Amount::from(amount_sats)),
            None,
            None,
        )
        .await?;

    let bolt11_str = quote.request.clone();
    let expires_at = quote.expiry;

    // Parse the bolt11 invoice to extract the payment hash
    let invoice = Bolt11Invoice::from_str(&bolt11_str)?;
    let hash_bytes: &[u8] = invoice.payment_hash().as_ref();
    let payment_hash = hex::encode(hash_bytes);

    // Spawn background task to wait for payment and mint tokens
    let wallet = mpp_state.wallet.clone();
    tokio::spawn(async move {
        let result = wallet
            .wait_and_mint_quote(
                quote,
                Default::default(),
                Default::default(),
                std::time::Duration::from_secs(500),
            )
            .await;

        match result {
            Ok(_) => tracing::info!("MPP invoice paid and tokens minted"),
            Err(e) => tracing::warn!("MPP invoice mint failed: {}", e),
        }
    });

    // Build the challenge request JSON
    let challenge_request = ChallengeRequest {
        amount: amount_sats.to_string(),
        currency: "sat".to_string(),
        description: Some(description.to_string()),
        method_details: LightningMethodDetails {
            invoice: bolt11_str,
            payment_hash: payment_hash.clone(),
            network: Some("mainnet".to_string()),
        },
    };

    // Serialize request to JSON and base64url-encode
    let request_json = serde_json::to_string(&challenge_request)?;
    let request_b64 = URL_SAFE_NO_PAD.encode(request_json.as_bytes());

    // Generate challenge ID
    let challenge_id = Uuid::new_v4().to_string();

    // Build expires as RFC 3339 timestamp
    let expires_rfc3339 = timestamp_to_rfc3339(expires_at);

    let challenge_params = ChallengeParams {
        id: challenge_id,
        realm: mpp_state.realm.clone(),
        method: "lightning".to_string(),
        intent: "charge".to_string(),
        request: request_b64,
        expires: Some(expires_rfc3339),
    };

    let challenge_state = ChallengeState {
        challenge_params: challenge_params.clone(),
        payment_hash,
        amount_sats,
        expires_at,
        consumed: false,
        query: query.to_string(),
    };

    let header_value = format_www_authenticate(&challenge_params);

    Ok((challenge_state, header_value))
}

/// Store a challenge state in the mint's KV store.
pub async fn store_challenge(mint: &Mint, state: &ChallengeState) -> anyhow::Result<()> {
    let value = serde_json::to_vec(state)?;
    let mut tx = mint.localstore().begin_transaction().await?;
    tx.kv_write(
        KV_PRIMARY_NAMESPACE,
        KV_SECONDARY_NAMESPACE,
        &state.challenge_params.id,
        &value,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Look up a challenge state from the mint's KV store.
pub async fn load_challenge(
    mint: &Mint,
    challenge_id: &str,
) -> anyhow::Result<Option<ChallengeState>> {
    let data = mint
        .localstore()
        .kv_read(KV_PRIMARY_NAMESPACE, KV_SECONDARY_NAMESPACE, challenge_id)
        .await?;

    match data {
        Some(bytes) => {
            let state: ChallengeState = serde_json::from_slice(&bytes)?;
            Ok(Some(state))
        }
        None => Ok(None),
    }
}

/// Atomically mark a challenge as consumed. Returns the challenge state if successful,
/// or None if the challenge was already consumed or not found.
pub async fn consume_challenge(
    mint: &Mint,
    challenge_id: &str,
) -> anyhow::Result<Option<ChallengeState>> {
    let mut tx = mint.localstore().begin_transaction().await?;

    let data = tx
        .kv_read(KV_PRIMARY_NAMESPACE, KV_SECONDARY_NAMESPACE, challenge_id)
        .await?;

    let Some(bytes) = data else {
        return Ok(None);
    };

    let mut state: ChallengeState = serde_json::from_slice(&bytes)?;

    if state.consumed {
        return Ok(None);
    }

    state.consumed = true;
    let value = serde_json::to_vec(&state)?;
    tx.kv_write(
        KV_PRIMARY_NAMESPACE,
        KV_SECONDARY_NAMESPACE,
        challenge_id,
        &value,
    )
    .await?;
    tx.commit().await?;

    Ok(Some(state))
}

// -- Credential parsing --

/// Parse an Authorization header value of the form "Payment <base64url>".
pub fn parse_credential(auth_header: &str) -> anyhow::Result<Credential> {
    let token = auth_header
        .strip_prefix("Payment ")
        .ok_or_else(|| anyhow::anyhow!("Authorization header must start with 'Payment '"))?;

    let decoded = URL_SAFE_NO_PAD.decode(token.trim())?;
    let credential: Credential = serde_json::from_slice(&decoded)?;
    Ok(credential)
}

// -- Verification --

/// Verify that sha256(preimage) == payment_hash.
/// Both preimage and payment_hash are hex-encoded strings.
pub fn verify_preimage(preimage: &str, payment_hash: &str) -> bool {
    // Preimage must be 64 hex chars (32 bytes)
    if preimage.len() != 64 {
        return false;
    }

    let preimage_bytes = match hex::decode(preimage) {
        Ok(b) => b,
        Err(_) => return false,
    };

    let expected_hash = match hex::decode(payment_hash) {
        Ok(b) => b,
        Err(_) => return false,
    };

    let mut hasher = Sha256::new();
    hasher.update(&preimage_bytes);
    let computed_hash = hasher.finalize();

    computed_hash.as_slice() == expected_hash.as_slice()
}

/// Verify that the echoed challenge fields match the stored challenge.
pub fn verify_challenge_echo(echoed: &ChallengeParams, stored: &ChallengeParams) -> bool {
    echoed.id == stored.id
        && echoed.realm == stored.realm
        && echoed.method == stored.method
        && echoed.intent == stored.intent
        && echoed.request == stored.request
        && echoed.expires == stored.expires
}

// -- Receipt generation --

/// Build a base64url-encoded receipt JSON string for the Payment-Receipt header.
pub fn build_receipt(challenge_id: &str, payment_hash: &str) -> String {
    let receipt = Receipt {
        method: "lightning".to_string(),
        challenge_id: challenge_id.to_string(),
        reference: payment_hash.to_string(),
        status: "success".to_string(),
        timestamp: timestamp_to_rfc3339(cdk::util::unix_time()),
    };

    let json = serde_json::to_string(&receipt).expect("Receipt serialization should not fail");
    URL_SAFE_NO_PAD.encode(json.as_bytes())
}

// -- Header formatting --

/// Format a ChallengeParams into a WWW-Authenticate header value.
///
/// Example output:
/// Payment id="abc123", realm="example.com", method="lightning", intent="charge", request="eyJ...", expires="2026-03-15T12:05:00Z"
pub fn format_www_authenticate(params: &ChallengeParams) -> String {
    let mut parts = vec![
        format!("Payment id=\"{}\"", params.id),
        format!("realm=\"{}\"", params.realm),
        format!("method=\"{}\"", params.method),
        format!("intent=\"{}\"", params.intent),
        format!("request=\"{}\"", params.request),
    ];

    if let Some(ref expires) = params.expires {
        parts.push(format!("expires=\"{}\"", expires));
    }

    parts.join(", ")
}

// -- Helpers --

/// Convert a unix timestamp to RFC 3339 format.
fn timestamp_to_rfc3339(unix_secs: u64) -> String {
    let secs = unix_secs as i64;
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Calculate date from days since epoch (1970-01-01)
    let (year, month, day) = days_to_date(days);

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hours, minutes, seconds
    )
}

/// Convert days since Unix epoch to (year, month, day).
fn days_to_date(days: i64) -> (i64, u32, u32) {
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}
