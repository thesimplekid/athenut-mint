use std::str::FromStr;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::header::{
    ACCESS_CONTROL_ALLOW_CREDENTIALS, ACCESS_CONTROL_ALLOW_ORIGIN, AUTHORIZATION, CONTENT_TYPE,
};
use axum::http::{HeaderMap, HeaderName, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use cdk::mint::Mint;
use cdk::mint_url::MintUrl;
use cdk::nuts::{PublicKey as CashuPublicKey, SecretKey, TokenV4};
use cdk::util::unix_time;
use reqwest::Client as ReqwestClient;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tower_http::cors::CorsLayer;

async fn get_info(State(state): State<ApiState>) -> Result<Json<Info>, StatusCode> {
    Ok(Json(state.info))
}

async fn get_search(
    headers: HeaderMap,
    q: Query<Params>,
    State(state): State<ApiState>,
) -> Result<Json<Vec<SearchResult>>, StatusCode> {
    let x_cashu = headers
        .get("X-Cashu")
        .ok_or(StatusCode::PAYMENT_REQUIRED)?
        .to_str()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let token: TokenV4 = TokenV4::from_str(x_cashu).unwrap();

    let token_amount = token.value().unwrap();

    let token_mint = token.mint_url.clone();

    if token_mint != state.settings.mint_url || token_amount != 1.into() {
        // All proofs must be from trusted mints
        return Err(StatusCode::PAYMENT_REQUIRED);
    }

    let proofs = token.proofs();
    let proof = proofs.first().ok_or(StatusCode::PAYMENT_REQUIRED)?;

    let time = unix_time();

    let mint = state.mint;

    mint.verify_proof(proof).await.map_err(|_| {
        tracing::warn!("P2PK verification failed");
        StatusCode::PAYMENT_REQUIRED
    })?;

    let y = proof.y().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    mint.check_ys_spendable(&[y], cdk::nuts::State::Spent)
        .await
        .map_err(|_| StatusCode::PAYMENT_REQUIRED)?;

    tracing::info!("Time to verify: {}", unix_time() - time);

    let time = unix_time();

    tracing::info!("Send: {}", unix_time() - time);

    // if unclaimed_count >= 50 {
    //     let wallet_clone = Arc::clone(&state.wallet);
    //     let unclaimed_proofs_clone = Arc::clone(&state.unclaimed_proofs);
    //     let secret_key_clone = state.settings.cashu_secret_key;
    //     let notification_pubkey = state.settings.nostr_pubkey;
    //     let nostr_relays = state.settings.nostr_relays.clone();

    //     tokio::spawn(async move {
    //         let mut proofs = unclaimed_proofs_clone.write().await;

    //         let count_to_swap = if proofs.len() > 50 { 50 } else { proofs.len() };

    //         let inputs_proofs = proofs.drain(..count_to_swap).collect();

    //         let amount = {
    //             let wallet = wallet_clone.lock().await;
    //             match wallet
    //                 .receive_proofs(
    //                     inputs_proofs,
    //                     SplitTarget::Value(1.into()),
    //                     &[secret_key_clone],
    //                     &[],
    //                 )
    //                 .await
    //             {
    //                 Ok(amount) => {
    //                     tracing::info!("Swapped {}", amount);
    //                     Some(amount)
    //                 }
    //                 Err(err) => {
    //                     tracing::error!("Could not swap proofs: {}", err);
    //                     None
    //                 }
    //             }
    //         };

    //         if let Some(amount) = amount {
    //             let my_keys = Keys::generate();
    //             let client = Client::new(my_keys);
    //             let msg = format!("Athenut just redeamed: {} search tokens", amount);

    //             for relay in nostr_relays {
    //                 if let Err(err) = client.add_write_relay(&relay).await {
    //                     tracing::error!("Could not add relay {}: {}", relay, err);
    //                 }
    //             }

    //             client.connect().await;

    //             if let Err(err) = client
    //                 .send_private_msg(notification_pubkey, msg, None)
    //                 .await
    //             {
    //                 tracing::error!("Could not send nostr notification: {}", err);
    //             }
    //         }
    //     });
    // }

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
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    tracing::info!("Kagi time: {}", unix_time() - time);
    let time = unix_time();
    let json_response = response
        .json::<Value>()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let results: KagiSearchResponse = serde_json::from_value(json_response).map_err(|_| {
        tracing::error!("Invalid response from kagi");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    tracing::info!(
        "fetched response: {} from {}",
        results.meta.ms,
        results.meta.node
    );

    let search_results: Vec<KagiSearchResult> = results
        .data
        .into_iter()
        .flat_map(|s| match s {
            KagiSearchObject::SearchResult(sr) => Some(sr),
            KagiSearchObject::RelatedSearches(_) => None,
        })
        .collect();

    let results: Vec<SearchResult> = search_results
        .into_iter()
        .flat_map(|r| r.try_into())
        .collect();

    tracing::info!("Json time: {}", unix_time() - time);
    Ok(Json(results))
}

pub fn search_router(state: ApiState) -> Router {
    Router::new()
        .route("/info", get(get_info))
        .route("/search", get(get_search))
        .layer(CorsLayer::very_permissive().allow_headers([
            AUTHORIZATION,
            CONTENT_TYPE,
            ACCESS_CONTROL_ALLOW_CREDENTIALS,
            ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderName::from_str("X-Cashu").unwrap(),
        ]))
        .with_state(state)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Params {
    q: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Info {
    pub mint: MintUrl,
    pub pubkey: CashuPublicKey,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub kagi_auth_token: String,
    pub mint_url: MintUrl,
    pub cashu_secret_key: SecretKey,
}

#[derive(Clone)]
pub struct ApiState {
    pub info: Info,
    pub mint: Arc<Mint>,
    pub settings: Settings,
    pub reqwest_client: ReqwestClient,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KagiSearchResponse {
    meta: Meta,
    data: Vec<KagiSearchObject>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
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