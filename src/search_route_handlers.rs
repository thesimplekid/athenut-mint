use std::str::FromStr;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use cdk::mint::Mint;
use cdk::mint_url::MintUrl;
use cdk::nuts::nut18::PaymentRequestBuilder;
use cdk::nuts::CurrencyUnit;
use cdk::nuts::TokenV4;
use cdk::util::unix_time;
use reqwest::Client as ReqwestClient;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::db::{Db, SearchCount};

async fn get_search_count(State(state): State<ApiState>) -> Result<Json<SearchCount>, StatusCode> {
    let db = state.db;

    let search_count = db
        .get_search_count()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

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
    let mint_url = match "https://mint.athenut.com".parse() {
        Ok(url) => url,
        Err(err) => {
            tracing::error!("Failed to parse mint URL: {}", err);
            let headers = HeaderMap::new();
            return Err((StatusCode::INTERNAL_SERVER_ERROR, headers));
        }
    };

    let payment_request = PaymentRequestBuilder::default()
        .unit(CurrencyUnit::Custom("xsr".to_string()))
        .amount(1)
        .add_mint(mint_url)
        .build();

    // Create payment required response with header
    let payment_required_response = || {
        let mut headers = HeaderMap::new();
        let header_value = payment_request
            .to_string()
            .parse()
            .map_err(|_| {
                tracing::error!("Failed to parse payment request to header value");
            })
            .expect("Valid header");
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

    let proofs = token.proofs();
    let proof = proofs.first().ok_or_else(payment_required_response)?;

    let time = unix_time();

    let mint = state.mint;

    mint.verify_proof(proof).await.map_err(|_| {
        tracing::warn!("P2PK verification failed");
        payment_required_response()
    })?;

    let y = proof.y().map_err(|_| {
        let headers = HeaderMap::new();
        (StatusCode::INTERNAL_SERVER_ERROR, headers)
    })?;

    mint.check_ys_spendable(&[y], cdk::nuts::State::Spent)
        .await
        .map_err(|_| payment_required_response())?;

    tracing::info!("Time to verify: {}", unix_time() - time);

    let time = unix_time();

    tracing::info!("Send: {}", unix_time() - time);

    let time = unix_time();

    let response = state
        .reqwest_client
        .get("https://kagi.com/api/v0/search")
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bot {}", state.settings.kagi_auth_token),
        )
        .query(&[("q", q.q.clone())])
        .send()
        .await
        .map_err(|err| {
            tracing::error!("Failed to make kagi request: {}", err);
            let headers = HeaderMap::new();
            (StatusCode::INTERNAL_SERVER_ERROR, headers)
        })?;

    tracing::info!("Kagi time: {}", unix_time() - time);
    let time = unix_time();
    let json_response = response.json::<Value>().await.map_err(|_| {
        let headers = HeaderMap::new();
        (StatusCode::INTERNAL_SERVER_ERROR, headers)
    })?;

    let results: KagiSearchResponse = serde_json::from_value(json_response).map_err(|_| {
        tracing::error!("Invalid response from kagi");
        let headers = HeaderMap::new();
        (StatusCode::INTERNAL_SERVER_ERROR, headers)
    })?;

    tracing::info!(
        "fetched response: {} from {}",
        results.meta.ms,
        results.meta.node
    );

    if let Err(err) = state.db.increment_search_count() {
        tracing::error!("Could not update search counter: {}", err);
    }

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
    pub db: Db,
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
