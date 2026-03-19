use std::str::FromStr;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use cdk::mint::Mint;
use cdk::mint_url::MintUrl;
use cdk::nuts::nut18::PaymentRequestBuilder;
use cdk::nuts::TokenV4;
use cdk::util::unix_time;
use cdk_common::melt::MeltQuoteRequest;
use cdk_common::MeltRequest;
use reqwest::Client as ReqwestClient;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::mpp::{self, MppState};
use crate::XSR_UNIT;

const KV_SEARCH_NAMESPACE: &str = "athenut";
const KV_CASHU_SEARCH_COUNT_KEY: &str = "cashu_search_count";
const KV_MPP_SEARCH_COUNT_KEY: &str = "mpp_search_count";

enum PaymentMethod {
    Cashu,
    Mpp,
}

async fn get_search_count(State(state): State<ApiState>) -> Result<Json<SearchCount>, StatusCode> {
    let mint = state.mint;

    let search_count = get_search_count_from_mint(&mint).await?;

    Ok(Json(search_count))
}

async fn get_info(State(state): State<ApiState>) -> Result<Json<Info>, StatusCode> {
    Ok(Json(state.info))
}

async fn get_agent_instructions(
    State(state): State<ApiState>,
) -> (StatusCode, HeaderMap, String) {
    let mint_url = &state.info.mint;
    let has_mpp = state.mpp_state.is_some();

    let mpp_section = if has_mpp {
        r#"

## MPP Lightning Payment Flow (Alternative)

This API also supports the Machine Payments Protocol (MPP) using Lightning Network BOLT11 invoices.

### Flow

1. Send `GET /search?q=<query>` without any payment headers.
2. The server responds with HTTP `402 Payment Required` with:
   - `X-Cashu` header: Cashu payment request (NUT-18 format)
   - `WWW-Authenticate: Payment ...` header: MPP Lightning charge challenge with a BOLT11 invoice
3. Choose one payment method:
   - **Option A (Cashu):** Pay with a Cashu token in the `X-Cashu` request header.
   - **Option B (MPP Lightning):** Pay the BOLT11 invoice from the challenge, then retry with `Authorization: Payment <credential>` header containing the payment preimage.
4. On success, the server responds with HTTP `200` and the search results as JSON.

### MPP Credential Format

The `Authorization` header carries a base64url-encoded JSON credential:

```json
{
  "challenge": { ... },  // Echo of the challenge from WWW-Authenticate
  "payload": {
    "preimage": "<64-char hex preimage>"
  }
}
```

The server verifies `sha256(preimage) == paymentHash` and returns the resource with a `Payment-Receipt` header.
"#
    } else {
        ""
    };

    let body = format!(
        r#"---
name: athenut-search
description: Private web search API that accepts payments via Cashu ecash tokens (X-Cashu) or MPP Lightning invoices (WWW-Authenticate: Payment)
metadata:
  type: http-api
  protocol: cashu-x-cashu, mpp-lightning
  unit: xsr
---

# Athenut Search - Paid Web Search API

Athenut is a private web search API. Each search costs 1 xsr (search result token). You pay by including a Cashu ecash token in the `X-Cashu` request header, or by paying a Lightning invoice via the MPP protocol.

The Cashu mint for this service is at: {mint_url}

## Agent Safety Policy (Required)

When acting as an AGENT with this skill:

- Always ask for explicit user permission before spending tokens on a search, unless the user has already clearly instructed you to perform the search.
- When receiving an HTTP 402 response, inspect the payment headers to understand the payment requirements before spending.
- Never expose or log token values, wallet credentials, or payment preimages.

## HTTP 402 Payment Flow (NUT-24 / X-Cashu)

This API uses the HTTP 402 Payment Required status with the `X-Cashu` header for payment negotiation.

### Flow

1. Send `GET /search?q=<query>` without an `X-Cashu` header (or with an invalid/insufficient token).
2. The server responds with HTTP `402 Payment Required` and an `X-Cashu` response header containing a Cashu payment request (NUT-18 format). This request specifies the mint URL, unit (`xsr`), and amount (`1`).
3. Use a Cashu wallet to create a token worth exactly 1 xsr minted from `{mint_url}`.
4. Retry the request with the `X-Cashu` header set to the token string (e.g. `cashuB...`).
5. On success, the server responds with HTTP `200` and the search results as JSON.

If you have a Cashu wallet skill (such as cocod), you can use its X-Cashu handling commands to parse and settle the 402 response automatically.
{mpp_section}
## Getting xsr Tokens

Use any Cashu wallet that supports custom units to mint xsr tokens from `{mint_url}`.

1. Create a mint quote for the desired amount of xsr from `{mint_url}`.
2. Pay the Lightning invoice returned by the mint.
3. Once the invoice is paid, mint the tokens.
4. Use the wallet's send function to create a Cashu token string (starts with `cashuB...`).

Each token worth 1 xsr pays for one search.

## Endpoints

### `GET /search`

Perform a web search. Requires payment via one of the supported methods.

**Query parameters:**
- `q` (required) - The search query string.

**Request headers (one of):**
- `X-Cashu` - A Cashu v4 token (starting with `cashuB...`) worth exactly 1 xsr, minted from `{mint_url}`.
- `Authorization: Payment <credential>` - An MPP Lightning credential with a valid payment preimage (base64url-encoded JSON).

**Success response (200):**

```json
[
  {{
    "url": "https://example.com",
    "title": "Example Result",
    "description": "A description of the result",
    "age": "2025-01-01T00:00:00Z"
  }}
]
```

**Payment required response (402):**

Returned when no payment is provided, the token is invalid, or the token amount is not exactly 1 xsr. Response includes:
- `X-Cashu` header: NUT-18 payment request specifying the mint, unit, and amount needed.
- `WWW-Authenticate: Payment ...` header: MPP Lightning charge challenge with a BOLT11 invoice (if MPP is enabled).
- `Cache-Control: no-store`

**Bad request response (400):**

Returned when the token cannot be parsed or the proofs are invalid.

### `GET /info`

Returns the mint URL for this service.

```json
{{
  "mint": "{mint_url}"
}}
```

### `GET /search_count`

Returns the all-time search count.

```json
{{
  "all_time_search_count": 12345
}}
```

### Cashu Mint Protocol (`/v1/*`)

Standard Cashu mint protocol endpoints are available under `/v1/`. These include key discovery, minting, melting, swaps, and other NUT operations. Use these if your wallet needs to interact with the mint directly.

## Example (Cashu)

```
GET /search?q=what+is+cashu HTTP/1.1
Host: {mint_url}
X-Cashu: cashuBo2F0...
```

## Example (MPP Lightning)

```
GET /search?q=what+is+cashu HTTP/1.1
Host: {mint_url}
Authorization: Payment eyJjaGFsbGVuZ2UiOns...
```

## Important Details

- Token must be worth exactly 1 xsr - no more, no less.
- The unit is `xsr`, not `sat` or any other standard unit.
- Tokens must be minted from `{mint_url}`.
- The `X-Cashu` header value is the raw token string (starting with `cashuB...`).
- For MPP, the `Authorization` header carries a base64url-encoded JSON credential with the payment preimage.

## Concepts

- **Cashu**: Privacy-preserving ecash protocol using blind signatures.
- **Mint**: Server that issues and redeems Cashu tokens. This service runs a mint at `{mint_url}`.
- **xsr**: Custom Cashu unit representing one search result. 1 xsr = 1 search.
- **Token**: A transferable Cashu string (starting with `cashuB...`) representing value.
- **X-Cashu**: HTTP header used to carry Cashu payment requests (server to client) and payment tokens (client to server).
- **MPP**: Machine Payments Protocol - an HTTP 402-based payment scheme using `WWW-Authenticate` and `Authorization` headers.
- **BOLT11**: Lightning Network invoice format used for MPP Lightning charge payments.
- **NUT-18**: Cashu protocol specification for payment requests.
- **NUT-24**: Cashu protocol specification for HTTP 402 payment flow using the X-Cashu header.
- **402 Payment Required**: HTTP status code indicating payment is needed before the request can be fulfilled.
"#,
        mint_url = mint_url,
        mpp_section = mpp_section
    );

    let mut headers = HeaderMap::new();
    headers.insert(
        "Content-Type",
        "text/plain; charset=utf-8".parse().unwrap(),
    );

    (StatusCode::OK, headers, body)
}

// -- Shared Kagi search helper --

/// Perform a Kagi web search and return parsed results.
async fn kagi_search(
    kagi_auth_token: &str,
    query: &str,
) -> Result<Vec<SearchResult>, (StatusCode, HeaderMap)> {
    let time = unix_time();

    let response = reqwest::Client::new()
        .get("https://kagi.com/api/v0/search")
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bot {}", kagi_auth_token),
        )
        .query(&[("q", query)])
        .send()
        .await
        .map_err(|e| {
            tracing::error!("Kagi request failed: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, HeaderMap::new())
        })?;

    let json_response: Value = response.json().await.map_err(|e| {
        tracing::error!("Failed to parse Kagi response: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, HeaderMap::new())
    })?;

    let results: KagiSearchResponse =
        serde_json::from_value(json_response).map_err(|e| {
            tracing::error!("Invalid response from Kagi: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, HeaderMap::new())
        })?;

    tracing::info!(
        "Kagi search completed: {}ms from {}",
        results.meta.ms,
        results.meta.node
    );

    let search_results: Vec<SearchResult> = results
        .data
        .into_iter()
        .flat_map(|s| match s {
            KagiSearchObject::SearchResult(sr) => Some(sr.into()),
            KagiSearchObject::RelatedSearches(_) => None,
        })
        .collect();

    tracing::info!("Kagi total time: {}", unix_time() - time);

    Ok(search_results)
}

// -- Main search handler --

async fn get_search(
    headers: HeaderMap,
    q: Query<Params>,
    State(state): State<ApiState>,
) -> Result<(StatusCode, HeaderMap, Json<Vec<SearchResult>>), (StatusCode, HeaderMap)> {
    // Check for MPP Authorization header first
    if let Some(auth_header) = headers.get("Authorization") {
        let auth_str = auth_header.to_str().map_err(|_| {
            tracing::error!("Invalid Authorization header encoding");
            (StatusCode::BAD_REQUEST, HeaderMap::new())
        })?;

        if auth_str.starts_with("Payment ") {
            return handle_mpp_payment(auth_str, &q.q, &state).await;
        }
    }

    // Check for Cashu X-Cashu header
    if let Some(x_cashu_header) = headers.get("X-Cashu") {
        let x_cashu = x_cashu_header.to_str().map_err(|_| {
            (StatusCode::INTERNAL_SERVER_ERROR, HeaderMap::new())
        })?;

        return handle_cashu_payment(x_cashu, &q.q, &state).await;
    }

    // No payment header present -- return 402 with both payment options
    build_payment_required_response(&q.q, &state).await
}

/// Handle payment via Cashu X-Cashu token (existing flow).
async fn handle_cashu_payment(
    x_cashu: &str,
    query: &str,
    state: &ApiState,
) -> Result<(StatusCode, HeaderMap, Json<Vec<SearchResult>>), (StatusCode, HeaderMap)> {
    let mint_url = state.info.mint.clone();

    let payment_required_response = || {
        let payment_request = PaymentRequestBuilder::default()
            .unit(XSR_UNIT.clone())
            .amount(1)
            .add_mint(mint_url.clone())
            .build();

        let mut headers = HeaderMap::new();
        let header_value = match payment_request.to_string().parse() {
            Ok(hv) => hv,
            Err(_) => {
                tracing::error!("Failed to parse payment request to header value");
                return (StatusCode::INTERNAL_SERVER_ERROR, headers);
            }
        };
        headers.insert("X-Cashu", header_value);
        (StatusCode::PAYMENT_REQUIRED, headers)
    };

    let token: TokenV4 = match TokenV4::from_str(x_cashu) {
        Ok(token) => token,
        Err(err) => {
            tracing::error!("Failed to parse token: {}", err);
            return Err((StatusCode::BAD_REQUEST, HeaderMap::new()));
        }
    };

    let token_amount = match token.value() {
        Ok(amount) => amount,
        Err(err) => {
            tracing::error!("Failed to get token value: {}", err);
            return Err((StatusCode::BAD_REQUEST, HeaderMap::new()));
        }
    };

    if token_amount != 1.into() {
        return Err(payment_required_response());
    }

    let melt_quote_request = MeltQuoteRequest::Custom(cdk_common::MeltQuoteCustomRequest {
        method: "bolt11".to_string(),
        request: query.to_string(),
        unit: XSR_UNIT.clone(),
        extra: Value::Null,
    });

    let mint = &state.mint;

    let quote = mint.get_melt_quote(melt_quote_request).await.map_err(|e| {
        tracing::error!("Failed to get melt quote: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, HeaderMap::new())
    })?;

    let keysets = mint.keysets().keysets;

    let proofs = token.proofs(&keysets).map_err(|e| {
        tracing::error!("Failed to get proofs from token: {}", e);
        (StatusCode::BAD_REQUEST, HeaderMap::new())
    })?;

    let proof = proofs
        .first()
        .cloned()
        .ok_or_else(payment_required_response)?;

    let time = unix_time();
    let melt_request = MeltRequest::new(quote.quote, vec![proof], None);
    tracing::info!("Time to verify: {}", unix_time() - time);

    let time = unix_time();
    let melt_res = mint.melt(&melt_request).await.map_err(|e| {
        tracing::error!("Failed to melt: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, HeaderMap::new())
    })?;

    tracing::info!("Kagi time: {}", unix_time() - time);

    let json_response = melt_res.payment_preimage.ok_or_else(|| {
        tracing::error!("Melt response missing preimage");
        (StatusCode::INTERNAL_SERVER_ERROR, HeaderMap::new())
    })?;

    let results: KagiSearchResponse = serde_json::from_str(&json_response).map_err(|_| {
        tracing::error!("Invalid response from kagi");
        (StatusCode::INTERNAL_SERVER_ERROR, HeaderMap::new())
    })?;

    tracing::info!(
        "fetched response: {} from {}",
        results.meta.ms,
        results.meta.node
    );

    let mint_clone = Arc::clone(mint);
    tokio::spawn(async move {
        if let Err(err) = add_search(&mint_clone, PaymentMethod::Cashu).await {
            tracing::error!("Could not update search counter: {}", err);
        }
    });

    let search_results: Vec<SearchResult> = results
        .data
        .into_iter()
        .flat_map(|s| match s {
            KagiSearchObject::SearchResult(sr) => Some(sr.into()),
            KagiSearchObject::RelatedSearches(_) => None,
        })
        .collect();

    Ok((StatusCode::OK, HeaderMap::new(), Json(search_results)))
}

/// Handle payment via MPP Lightning charge credential.
async fn handle_mpp_payment(
    auth_header: &str,
    query: &str,
    state: &ApiState,
) -> Result<(StatusCode, HeaderMap, Json<Vec<SearchResult>>), (StatusCode, HeaderMap)> {
    let _mpp_state = state.mpp_state.as_ref().ok_or_else(|| {
        tracing::error!("MPP payment received but MPP is not enabled");
        (StatusCode::BAD_REQUEST, HeaderMap::new())
    })?;

    // Parse the credential from the Authorization header
    let credential = mpp::parse_credential(auth_header).map_err(|e| {
        tracing::error!("Failed to parse MPP credential: {}", e);
        (StatusCode::BAD_REQUEST, HeaderMap::new())
    })?;

    let challenge_id = credential.challenge.id.clone();

    // Look up and atomically consume the challenge
    let challenge_state = mpp::consume_challenge(&state.mint, &challenge_id)
        .await
        .map_err(|e| {
            tracing::error!("Failed to consume MPP challenge: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, HeaderMap::new())
        })?
        .ok_or_else(|| {
            tracing::warn!(
                "MPP challenge not found or already consumed: {}",
                challenge_id
            );
            (StatusCode::BAD_REQUEST, HeaderMap::new())
        })?;

    // Verify the echoed challenge matches the stored challenge
    if !mpp::verify_challenge_echo(&credential.challenge, &challenge_state.challenge_params) {
        tracing::warn!("MPP challenge echo mismatch for: {}", challenge_id);
        return Err((StatusCode::BAD_REQUEST, HeaderMap::new()));
    }

    // Check expiry
    let now = unix_time();
    if now > challenge_state.expires_at {
        tracing::warn!("MPP challenge expired: {}", challenge_id);
        return Err((StatusCode::BAD_REQUEST, HeaderMap::new()));
    }

    // Verify sha256(preimage) == payment_hash
    if !mpp::verify_preimage(&credential.payload.preimage, &challenge_state.payment_hash) {
        tracing::warn!("MPP preimage verification failed for: {}", challenge_id);
        return Err((StatusCode::BAD_REQUEST, HeaderMap::new()));
    }

    // Verify the query matches the one the challenge was issued for
    if query != challenge_state.query {
        tracing::warn!(
            "MPP query mismatch for challenge {}: expected '{}', got '{}'",
            challenge_id,
            challenge_state.query,
            query
        );
        return Err((StatusCode::BAD_REQUEST, HeaderMap::new()));
    }

    tracing::info!("MPP payment verified for challenge: {}", challenge_id);

    // Perform the Kagi search directly
    let results = kagi_search(&state.settings.kagi_auth_token, query).await?;

    // Update search counter
    let mint_clone = Arc::clone(&state.mint);
    tokio::spawn(async move {
        if let Err(err) = add_search(&mint_clone, PaymentMethod::Mpp).await {
            tracing::error!("Could not update search counter: {}", err);
        }
    });

    // Build the Payment-Receipt header
    let receipt_value = mpp::build_receipt(&challenge_id, &challenge_state.payment_hash);
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        "Payment-Receipt",
        receipt_value.parse().unwrap_or_else(|_| {
            tracing::error!("Failed to format Payment-Receipt header");
            "".parse().unwrap()
        }),
    );

    Ok((StatusCode::OK, response_headers, Json(results)))
}

/// Build a 402 Payment Required response with both Cashu and MPP payment options.
async fn build_payment_required_response(
    query: &str,
    state: &ApiState,
) -> Result<(StatusCode, HeaderMap, Json<Vec<SearchResult>>), (StatusCode, HeaderMap)> {
    let mint_url = state.info.mint.clone();

    let payment_request = PaymentRequestBuilder::default()
        .unit(XSR_UNIT.clone())
        .amount(1)
        .add_mint(mint_url)
        .build();

    let mut headers = HeaderMap::new();

    // Add X-Cashu header (Cashu payment request)
    let cashu_header_value = match payment_request.to_string().parse() {
        Ok(hv) => hv,
        Err(_) => {
            tracing::error!("Failed to parse payment request to header value");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, HeaderMap::new()));
        }
    };
    headers.insert("X-Cashu", cashu_header_value);

    // Add WWW-Authenticate header (MPP Lightning challenge) if MPP is enabled
    if let Some(mpp_state) = &state.mpp_state {
        match mpp::generate_challenge(mpp_state, "Athenut web search", query).await {
            Ok((challenge_state, www_auth_value)) => {
                // Store the challenge state in the KV store
                if let Err(e) = mpp::store_challenge(&state.mint, &challenge_state).await {
                    tracing::error!("Failed to store MPP challenge: {}", e);
                } else {
                    match www_auth_value.parse() {
                        Ok(hv) => {
                            headers.insert("WWW-Authenticate", hv);
                        }
                        Err(e) => {
                            tracing::error!("Failed to parse WWW-Authenticate header value: {}", e);
                        }
                    }
                }
            }
            Err(e) => {
                tracing::error!("Failed to generate MPP challenge: {}", e);
                // Continue without MPP -- Cashu is still available
            }
        }
    }

    // Add Cache-Control: no-store per the spec
    headers.insert("Cache-Control", "no-store".parse().unwrap());

    Err((StatusCode::PAYMENT_REQUIRED, headers))
}

#[derive(Debug, Clone, Copy, Hash, Serialize, Deserialize)]
pub struct SearchCount {
    pub cashu_search_count: u64,
    pub mpp_search_count: u64,
}

async fn add_search(mint: &Mint, payment_method: PaymentMethod) -> anyhow::Result<()> {
    let kv_key = match payment_method {
        PaymentMethod::Cashu => KV_CASHU_SEARCH_COUNT_KEY,
        PaymentMethod::Mpp => KV_MPP_SEARCH_COUNT_KEY,
    };

    let mut tx = mint.localstore().begin_transaction().await?;

    let current_count = tx
        .kv_read(KV_SEARCH_NAMESPACE, "count", kv_key)
        .await?
        .map(|v| {
            let bytes = v.as_slice();
            u64::from_le_bytes(bytes.try_into().unwrap_or([0; 8]))
        })
        .unwrap_or(0);

    let new_count = current_count + 1;
    let value = new_count.to_le_bytes().to_vec();

    tx.kv_write(KV_SEARCH_NAMESPACE, "count", kv_key, &value)
        .await?;

    tx.commit().await?;

    Ok(())
}

async fn get_search_count_from_mint(mint: &Mint) -> Result<SearchCount, StatusCode> {
    let store = mint.localstore();

    let cashu_count = store
        .kv_read(KV_SEARCH_NAMESPACE, "count", KV_CASHU_SEARCH_COUNT_KEY)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(|v| {
            let bytes = v.as_slice();
            u64::from_le_bytes(bytes.try_into().unwrap_or([0; 8]))
        })
        .unwrap_or(0);

    let mpp_count = store
        .kv_read(KV_SEARCH_NAMESPACE, "count", KV_MPP_SEARCH_COUNT_KEY)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(|v| {
            let bytes = v.as_slice();
            u64::from_le_bytes(bytes.try_into().unwrap_or([0; 8]))
        })
        .unwrap_or(0);

    Ok(SearchCount {
        cashu_search_count: cashu_count,
        mpp_search_count: mpp_count,
    })
}

pub fn search_router(state: ApiState) -> Router {
    Router::new()
        .route("/info", get(get_info))
        .route("/search", get(get_search))
        .route("/search_count", get(get_search_count))
        .route("/agent", get(get_agent_instructions))
        .with_state(state)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Params {
    q: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Info {
    pub mint: MintUrl,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub kagi_auth_token: String,
    pub mint_url: MintUrl,
}

#[derive(Clone)]
pub struct ApiState {
    pub info: Info,
    pub mint: Arc<Mint>,
    pub settings: Settings,
    pub reqwest_client: ReqwestClient,
    pub mpp_state: Option<Arc<MppState>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct KagiSearchResponse {
    meta: Meta,
    data: Vec<KagiSearchObject>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Meta {
    id: String,
    node: String,
    ms: u64,
    api_balance: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SearchResult {
    url: String,
    title: String,
    description: Option<String>,
    age: Option<String>,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
enum KagiSearchObject {
    SearchResult(KagiSearchResult),
    RelatedSearches(KagiRelatedSearches),
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
struct KagiSearchResult {
    t: u8,
    rank: Option<u64>,
    url: String,
    title: String,
    snippet: Option<String>,
    published: Option<String>,
    image: Option<Image>,
    list: Option<Vec<String>>,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
struct Image {
    url: String,
    height: u64,
    width: u64,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
struct KagiRelatedSearches {
    t: u8,
    list: Vec<String>,
}

impl From<KagiSearchResult> for SearchResult {
    fn from(kagi: KagiSearchResult) -> SearchResult {
        SearchResult {
            url: kagi.url,
            title: kagi.title,
            description: kagi.snippet,
            age: kagi.published,
        }
    }
}
