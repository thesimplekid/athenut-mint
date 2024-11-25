use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use anyhow::{anyhow, bail};
use athenut_mint::cli::CLIArgs;
use athenut_mint::cln::Cln;
use athenut_mint::db::Db;
use athenut_mint::search_route_handlers::{search_router, ApiState};
use athenut_mint::{config, expand_path, work_dir};
use axum::Router;
use bip39::Mnemonic;
use bitcoin::bip32::{ChildNumber, DerivationPath};
use cdk::cdk_lightning::{self, MintLightning};
use cdk::mint::{FeeReserve, Mint};
use cdk::mint_url::MintUrl;
use cdk::nuts::{
    nut04, nut05, ContactInfo, CurrencyUnit, MeltMethodSettings, MintInfo, MintMethodSettings,
    MintVersion, Nuts, PaymentMethod,
};
use cdk::types::{LnKey, QuoteTTL};
use cdk_redb::MintRedbDatabase;
use clap::Parser;
use reqwest::Client;
use tokio::sync::Notify;
use tower_http::cors::CorsLayer;
use tracing_subscriber::EnvFilter;

const CARGO_PKG_VERSION: Option<&'static str> = option_env!("CARGO_PKG_VERSION");
const DEFAULT_QUOTE_TTL_SECS: u64 = 1800;
const DEFAULT_CACHE_TTL_SECS: u64 = 1800;
const DEFAULT_CACHE_TTI_SECS: u64 = 1800;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let default_filter = "debug";

    let sqlx_filter = "sqlx=warn";
    let hyper_filter = "hyper=warn";

    let env_filter = EnvFilter::new(format!(
        "{},{},{}",
        default_filter, sqlx_filter, hyper_filter
    ));

    tracing_subscriber::fmt().with_env_filter(env_filter).init();

    let args = CLIArgs::parse();

    let work_dir = match args.work_dir {
        Some(w) => w,
        None => work_dir()?,
    };

    let redb_path = work_dir.join("cdk-mintd.redb");
    let localstore = Arc::new(MintRedbDatabase::new(&redb_path)?);

    let mint_version = MintVersion::new(
        "cdk-athenut-mint".to_string(),
        CARGO_PKG_VERSION.unwrap_or("Unknown").to_string(),
    );

    // get config file name from args
    let config_file_arg = match args.config {
        Some(c) => c,
        None => work_dir.join("config.toml"),
    };

    let settings = config::Settings::new(&Some(config_file_arg));

    let mut contact_info: Option<Vec<ContactInfo>> = None;

    if let Some(nostr_contact) = &settings.mint_info.contact_nostr_public_key {
        let nostr_contact = ContactInfo::new("nostr".to_string(), nostr_contact.to_string());

        contact_info = match contact_info {
            Some(mut vec) => {
                vec.push(nostr_contact);
                Some(vec)
            }
            None => Some(vec![nostr_contact]),
        };
    }

    if let Some(email_contact) = &settings.mint_info.contact_email {
        let email_contact = ContactInfo::new("email".to_string(), email_contact.to_string());

        contact_info = match contact_info {
            Some(mut vec) => {
                vec.push(email_contact);
                Some(vec)
            }
            None => Some(vec![email_contact]),
        };
    }

    let relative_ln_fee = settings.ln.fee_percent;

    let absolute_ln_fee_reserve = settings.ln.reserve_fee_min;

    let fee_reserve = FeeReserve {
        min_fee_reserve: absolute_ln_fee_reserve,
        percent_fee_reserve: relative_ln_fee,
    };

    let mut ln_backends: HashMap<
        LnKey,
        Arc<dyn MintLightning<Err = cdk_lightning::Error> + Send + Sync>,
    > = HashMap::new();

    let mut supported_units = HashMap::new();

    let cln_socket = expand_path(
        settings
            .cln
            .rpc_path
            .to_str()
            .ok_or(anyhow!("cln socket not defined"))?,
    )
    .ok_or(anyhow!("cln socket not defined"))?;

    let cln = Arc::new(Cln::new(cln_socket, fee_reserve).await?);

    let search_unit = CurrencyUnit::from_str("XSR")?;
    ln_backends.insert(LnKey::new(search_unit.clone(), PaymentMethod::Bolt11), cln);
    supported_units.insert(search_unit.clone(), (0, 1));

    let nut04_settings = nut04::Settings::new(
        vec![MintMethodSettings {
            method: PaymentMethod::Bolt11,
            unit: search_unit.clone(),
            min_amount: Some(1.into()),
            max_amount: Some(100.into()),
            description: true,
        }],
        false,
    );

    let nut05_settings = nut05::Settings::new(
        vec![MeltMethodSettings {
            method: PaymentMethod::Bolt11,
            unit: search_unit.clone(),
            min_amount: None,
            max_amount: None,
        }],
        true,
    );

    let nuts = Nuts::new()
        .nut04(nut04_settings)
        .nut05(nut05_settings)
        .nut07(true)
        .nut08(true)
        .nut09(true)
        .nut10(true)
        .nut11(true)
        .nut12(true)
        .nut14(true);

    let mut mint_info = MintInfo::new()
        .name(settings.mint_info.name)
        .version(mint_version)
        .description(settings.mint_info.description)
        .nuts(nuts);

    if let Some(long_description) = &settings.mint_info.description_long {
        mint_info = mint_info.long_description(long_description);
    }

    if let Some(contact_info) = contact_info {
        mint_info = mint_info.contact_info(contact_info);
    }

    if let Some(pubkey) = settings.mint_info.pubkey {
        mint_info = mint_info.pubkey(pubkey);
    }

    if let Some(icon_url) = &settings.mint_info.icon_url {
        mint_info = mint_info.icon_url(icon_url);
    }

    if let Some(motd) = settings.mint_info.motd {
        mint_info = mint_info.motd(motd);
    }

    let quote_ttl = QuoteTTL::new(DEFAULT_QUOTE_TTL_SECS, DEFAULT_QUOTE_TTL_SECS);

    let search_der_path = DerivationPath::from(vec![
        ChildNumber::from_hardened_idx(0).expect("0 is a valid index"),
        ChildNumber::from_hardened_idx(4).expect("0 is a valid index"),
        ChildNumber::from_hardened_idx(0).expect("0 is a valid index"),
    ]);

    let mut custom_ders = HashMap::new();

    custom_ders.insert(search_unit, search_der_path);

    let mnemonic = Mnemonic::from_str(&settings.info.mnemonic)?;

    let mint = Mint::new(
        &settings.info.url,
        &mnemonic.to_seed_normalized(""),
        mint_info,
        quote_ttl,
        localstore,
        ln_backends.clone(),
        supported_units,
        custom_ders,
    )
    .await?;

    let mint = Arc::new(mint);

    let listen_addr = settings.info.listen_host;
    let listen_port = settings.info.listen_port;

    let cache_ttl = settings
        .info
        .seconds_to_cache_requests_for
        .unwrap_or(DEFAULT_CACHE_TTL_SECS);
    let cache_tti = settings
        .info
        .seconds_to_extend_cache_by
        .unwrap_or(DEFAULT_CACHE_TTI_SECS);

    let v1_service = cdk_axum::create_mint_router(Arc::clone(&mint), cache_ttl, cache_tti).await?;

    // Database for athenmint
    let athenmint_db = work_dir.join("athenmint_search_api.redb");
    let db = Db::new(&athenmint_db)?;

    let mint_url = MintUrl::from_str(&settings.info.url)?;
    let info = athenut_mint::search_route_handlers::Info {
        mint: mint_url.clone(),
    };

    let search_settings = athenut_mint::search_route_handlers::Settings {
        kagi_auth_token: settings.search_settings.kagi_auth_token,
        mint_url,
    };

    let api_state = ApiState {
        info,
        mint: Arc::clone(&mint),
        settings: search_settings,
        reqwest_client: Client::new(),
        db,
    };

    let search_router = search_router(api_state);

    let mint_service = Router::new()
        .merge(v1_service)
        .merge(search_router)
        .layer(CorsLayer::permissive());

    let shutdown = Arc::new(Notify::new());

    tokio::spawn({
        let shutdown = Arc::clone(&shutdown);
        async move { mint.wait_for_paid_invoices(shutdown).await }
    });

    let axum_result = axum::Server::bind(
        &format!("{}:{}", listen_addr, listen_port)
            .as_str()
            .parse()?,
    )
    .serve(mint_service.into_make_service())
    .await;

    shutdown.notify_waiters();

    match axum_result {
        Ok(_) => {
            tracing::info!("Axum server stopped with okay status");
        }
        Err(err) => {
            tracing::warn!("Axum server stopped with error");
            tracing::error!("{}", err);

            bail!("Axum exited with error")
        }
    }

    Ok(())
}
