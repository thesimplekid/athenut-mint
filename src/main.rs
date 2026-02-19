use std::collections::HashMap;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;

use anyhow::{anyhow, bail};
use athenut_mint::cdk_wallet::CashuWalletBackend;
use athenut_mint::cli::CLIArgs;
use athenut_mint::search_route_handlers::{search_router, ApiState};
use athenut_mint::{config, work_dir, XSR_UNIT};
use axum::Router;
use bip39::Mnemonic;
use bitcoin::bip32::{ChildNumber, DerivationPath};
use cdk::mint::{MintBuilder, MintMeltLimits};
use cdk::mint_url::MintUrl;
use cdk::nuts::{ContactInfo, MintVersion, PaymentMethod};
use cdk::types::QuoteTTL;
use cdk_common::payment::DynMintPayment;
use cdk_sqlite::MintSqliteDatabase;
use clap::Parser;
use reqwest::Client;
use tower_http::cors::CorsLayer;
use tracing_subscriber::EnvFilter;

const CARGO_PKG_VERSION: Option<&'static str> = option_env!("CARGO_PKG_VERSION");
const DEFAULT_QUOTE_TTL_SECS: u64 = 1800;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let default_filter = "debug";

    let sqlx_filter = "sqlx=warn";
    let hyper_filter = "hyper=warn";
    let rustls_filter = "rustls=warn,tungstenite=warn,tokio_tungstenite=warn";

    let env_filter = EnvFilter::new(format!(
        "{},{},{},{}",
        default_filter, sqlx_filter, hyper_filter, rustls_filter
    ));

    tracing_subscriber::fmt().with_env_filter(env_filter).init();

    let args = CLIArgs::parse();

    let work_dir = match args.work_dir {
        Some(w) => w,
        None => work_dir()?,
    };

    std::fs::create_dir_all(&work_dir)?;

    let sqlite_path = work_dir.join("cdk-mintd.sqlite");
    let localstore = Arc::new(MintSqliteDatabase::new(&sqlite_path).await?);

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

    let mut mint_builder = MintBuilder::new(localstore.clone());

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

    let mut supported_units = HashMap::new();
    let search_unit = XSR_UNIT.clone();

    let cashu_wallet_seed = settings
        .cashu_wallet
        .seed
        .as_ref()
        .ok_or(anyhow!("cashu_wallet seed not defined"))?;

    let cdk_wallet_backend = CashuWalletBackend::new(
        &settings.cashu_wallet.mint_url,
        cashu_wallet_seed,
        &work_dir,
        &settings.search_settings.kagi_auth_token,
        settings.cashu_wallet.cost_per_xsr_cents,
    )
    .await?;

    let cdk_wallet_backend: DynMintPayment = Arc::new(cdk_wallet_backend);

    supported_units.insert(search_unit.clone(), (0, 1));

    let mint_melt_limits = MintMeltLimits {
        mint_min: 1.into(),
        mint_max: 50.into(),
        melt_min: 1.into(),
        melt_max: 1.into(),
    };

    mint_builder.configure_unit(
        search_unit.clone(),
        cdk::mint::UnitConfig {
            amounts: vec![1],
            input_fee_ppk: 0,
        },
    )?;

    mint_builder
        .add_payment_processor(
            search_unit.clone(),
            PaymentMethod::BOLT11,
            mint_melt_limits,
            cdk_wallet_backend,
        )
        .await?;

    if let Some(long_description) = &settings.mint_info.description_long {
        mint_builder = mint_builder.with_long_description(long_description.to_string());
    }

    if let Some(contact_info) = contact_info {
        for info in contact_info {
            mint_builder = mint_builder.with_contact_info(info);
        }
    }

    if let Some(pubkey) = settings.mint_info.pubkey {
        mint_builder = mint_builder.with_pubkey(pubkey);
    }

    if let Some(icon_url) = &settings.mint_info.icon_url {
        mint_builder = mint_builder.with_icon_url(icon_url.to_string());
    }

    if let Some(motd) = settings.mint_info.motd {
        mint_builder = mint_builder.with_motd(motd);
    }

    let mnemonic = Mnemonic::from_str(&settings.info.mnemonic)?;

    let search_der_path = DerivationPath::from(vec![
        ChildNumber::from_hardened_idx(0).expect("0 is a valid index"),
        ChildNumber::from_hardened_idx(25).expect("0 is a valid index"),
        ChildNumber::from_hardened_idx(0).expect("0 is a valid index"),
    ]);

    let mut custom_ders = HashMap::new();
    custom_ders.insert(search_unit, search_der_path);

    let mint = mint_builder
        .with_name(settings.mint_info.name)
        .with_version(mint_version)
        .with_description(settings.mint_info.description)
        .with_custom_derivation_paths(custom_ders)
        .build_with_seed(localstore.clone(), &mnemonic.to_seed_normalized(""))
        .await?;

    let quote_ttl = QuoteTTL::new(DEFAULT_QUOTE_TTL_SECS, DEFAULT_QUOTE_TTL_SECS);
    mint.set_quote_ttl(quote_ttl).await?;

    let mint = Arc::new(mint);

    let listen_addr = settings.info.listen_host;
    let listen_port = settings.info.listen_port;

    let v1_service =
        cdk_axum::create_mint_router(Arc::clone(&mint), vec![PaymentMethod::BOLT11.to_string()])
            .await?;

    mint.start().await?;

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
    };

    let search_router = search_router(api_state);

    let mint_service = Router::new()
        .merge(v1_service)
        .merge(search_router)
        .layer(CorsLayer::very_permissive());

    let socket_addr = SocketAddr::from_str(&format!("{}:{}", listen_addr, listen_port))?;

    let listener = tokio::net::TcpListener::bind(socket_addr).await?;

    tracing::debug!("listening on {}", listener.local_addr()?);

    let axum_result = axum::serve(listener, mint_service).with_graceful_shutdown(shutdown_signal());

    match axum_result.await {
        Ok(_) => {
            tracing::info!("Axum server stopped with okay status");
        }
        Err(err) => {
            tracing::warn!("Axum server stopped with error");
            tracing::error!("{}", err);
            bail!("Axum exited with error")
        }
    }
    mint.stop().await?;

    Ok(())
}

async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("failed to install CTRL+C handler");
    tracing::info!("Shutdown signal received");
}
