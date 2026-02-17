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

use crate::XSR_UNIT;

const KV_SEARCH_NAMESPACE: &str = "athenut";
const KV_SEARCH_COUNT_KEY: &str = "search_count";

async fn get_search_count(State(state): State<ApiState>) -> Result<Json<SearchCount>, StatusCode> {
    let mint = state.mint;

    let search_count = get_search_count_from_mint(&mint).await?;

    Ok(Json(search_count))
}

async fn get_info(State(state): State<ApiState>) -> Result<Json<Info>, StatusCode> {
    Ok(Json(state.info))
}

async fn get_search(
    headers: HeaderMap,
    q: Query<Params>,
    State(state): State<ApiState>,
) -> Result<Json<Vec<SearchResult>>, (StatusCode, HeaderMap)> {
    let mint_url = state.info.mint;

    let payment_request = PaymentRequestBuilder::default()
        .unit(XSR_UNIT.clone())
        .amount(1)
        .add_mint(mint_url)
        .build();

    // Create payment required response with header
    let payment_required_response = || {
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

    let x_cashu = headers
        .get("X-Cashu")
        .ok_or_else(payment_required_response)?
        .to_str()
        .map_err(|_| {
            let headers = HeaderMap::new();
            (StatusCode::INTERNAL_SERVER_ERROR, headers)
        })?;

    let token: TokenV4 = match TokenV4::from_str(x_cashu) {
        Ok(token) => token,
        Err(err) => {
            tracing::error!("Failed to parse token: {}", err);
            let headers = HeaderMap::new();
            return Err((StatusCode::BAD_REQUEST, headers));
        }
    };

    let token_amount = match token.value() {
        Ok(amount) => amount,
        Err(err) => {
            tracing::error!("Failed to get token value: {}", err);
            let headers = HeaderMap::new();
            return Err((StatusCode::BAD_REQUEST, headers));
        }
    };

    if token_amount != 1.into() {
        return Err(payment_required_response());
    }

    let melt_quote_request = MeltQuoteRequest::Custom(cdk_common::MeltQuoteCustomRequest {
        method: "bolt11".to_string(),
        request: q.q.clone(),
        unit: XSR_UNIT.clone(),
        extra: Value::Null,
    });

    let mint = state.mint;

    let quote = mint.get_melt_quote(melt_quote_request).await.map_err(|e| {
        tracing::error!("Failed to get melt quote: {}", e);
        let headers = HeaderMap::new();
        (StatusCode::INTERNAL_SERVER_ERROR, headers)
    })?;

    let keysets = mint.keysets().keysets;

    // REVIEW: I think mint keysets is only the active ones and we should have old ones too
    let proofs = token.proofs(&keysets).map_err(|e| {
        tracing::error!("Failed to get proofs from token: {}", e);
        let headers = HeaderMap::new();
        (StatusCode::BAD_REQUEST, headers)
    })?;

    let proof = proofs
        .first()
        .cloned()
        .ok_or_else(payment_required_response)?;

    let time = unix_time();

    let melt_request = MeltRequest::new(quote.quote, vec![proof], None);

    tracing::info!("Time to verify: {}", unix_time() - time);

    let time = unix_time();

    tracing::info!("Send: {}", unix_time() - time);

    let time = unix_time();
    let melt_res = mint.melt(&melt_request).await.map_err(|e| {
        tracing::error!("Failed to melt: {}", e);
        let headers = HeaderMap::new();
        (StatusCode::INTERNAL_SERVER_ERROR, headers)
    })?;

    tracing::info!("Kagi time: {}", unix_time() - time);
    let time = unix_time();

    let json_response = melt_res.payment_preimage.ok_or_else(|| {
        tracing::error!("Melt response missing preimage");
        let headers = HeaderMap::new();
        (StatusCode::INTERNAL_SERVER_ERROR, headers)
    })?;

    let results: KagiSearchResponse = serde_json::from_str(&json_response).map_err(|_| {
        tracing::error!("Invalid response from kagi");
        let headers = HeaderMap::new();
        (StatusCode::INTERNAL_SERVER_ERROR, headers)
    })?;

    tracing::info!(
        "fetched response: {} from {}",
        results.meta.ms,
        results.meta.node
    );

    let mint_clone = Arc::clone(&mint);
    tokio::spawn(async move {
        if let Err(err) = add_search(&mint_clone).await {
            tracing::error!("Could not update search counter: {}", err);
        }
    });

    let search_results: Vec<KagiSearchResult> = results
        .data
        .into_iter()
        .flat_map(|s| match s {
            KagiSearchObject::SearchResult(sr) => Some(sr),
            KagiSearchObject::RelatedSearches(_) => None,
        })
        .collect();

    let results: Vec<SearchResult> = search_results.into_iter().map(|r| r.into()).collect();

    tracing::info!("Json time: {}", unix_time() - time);
    Ok(Json(results))
}

#[derive(Debug, Clone, Copy, Hash, Serialize, Deserialize)]
pub struct SearchCount {
    pub all_time_search_count: u64,
}

pub async fn add_search(mint: &Mint) -> anyhow::Result<()> {
    let mut tx = mint.localstore().begin_transaction().await?;

    let current_count = tx
        .kv_read(KV_SEARCH_NAMESPACE, "count", KV_SEARCH_COUNT_KEY)
        .await?
        .map(|v| {
            let bytes = v.as_slice();
            u64::from_le_bytes(bytes.try_into().unwrap_or([0; 8]))
        })
        .unwrap_or(0);

    let new_count = current_count + 1;
    let value = new_count.to_le_bytes().to_vec();

    tx.kv_write(KV_SEARCH_NAMESPACE, "count", KV_SEARCH_COUNT_KEY, &value)
        .await?;

    tx.commit().await?;

    Ok(())
}

async fn get_search_count_from_mint(mint: &Mint) -> Result<SearchCount, StatusCode> {
    let count = mint
        .localstore()
        .kv_read(KV_SEARCH_NAMESPACE, "count", KV_SEARCH_COUNT_KEY)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(|v| {
            let bytes = v.as_slice();
            u64::from_le_bytes(bytes.try_into().unwrap_or([0; 8]))
        })
        .unwrap_or(0);

    Ok(SearchCount {
        all_time_search_count: count,
    })
}

pub fn search_router(state: ApiState) -> Router {
    Router::new()
        .route("/info", get(get_info))
        .route("/search", get(get_search))
        .route("/search_count", get(get_search_count))
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
