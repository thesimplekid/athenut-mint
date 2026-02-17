use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cdk::nuts::CurrencyUnit;
use cdk::nuts::PaymentMethod;
use cdk::wallet::Wallet;
use cdk::Amount;
use cdk_common::amount::SplitTarget;
use cdk_sqlite::WalletSqliteDatabase;
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use futures_core::Stream;
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use cdk_common::nuts::CurrencyUnit as CommonCurrencyUnit;
use cdk_common::payment::{
    Bolt11Settings, CreateIncomingPaymentResponse, Event, IncomingPaymentOptions,
    MakePaymentResponse, MintPayment, OutgoingPaymentOptions, PaymentIdentifier,
    PaymentQuoteResponse, SettingsResponse, WaitPaymentResponse,
};
use cdk_common::Amount as CommonAmount;
use serde::Deserialize;
use serde::Serialize;
use uuid::Uuid;

use crate::{XSR_COMMON_UNIT, XSR_UNIT};

const KV_PRIMARY_NAMESPACE: &str = "athenut";
const KV_SECONDARY_NAMESPACE: &str = "incoming_payment";

#[derive(Serialize, Deserialize)]
struct IncomingPaymentInfo {
    cost_sats: u64,
    amount_xsr: u64,
}

pub struct CashuWalletBackend {
    wallet: Arc<Wallet>,
    wait_invoice_active: Arc<AtomicBool>,
    pending_mints: Arc<Mutex<FuturesUnordered<JoinHandle<Option<WaitPaymentResponse>>>>>,
    kagi_auth_token: String,
    cost_per_xsr_cents: u64,
}

impl CashuWalletBackend {
    pub async fn new(
        mint_url: &str,
        mnemonic: &str,
        home_dir: &Path,
        kagi_auth_token: &str,
        cost_per_xsr_cents: u64,
    ) -> anyhow::Result<Self> {
        let mnemonic = bip39::Mnemonic::parse(mnemonic)
            .map_err(|e| anyhow::anyhow!("Invalid mnemonic: {}", e))?;
        let seed = mnemonic.to_seed("");

        let db_path = home_dir.join("cdk_wallet.sqlite");
        let localstore = WalletSqliteDatabase::new(&db_path).await?;

        let wallet = Wallet::new(
            mint_url,
            CurrencyUnit::Sat,
            Arc::new(localstore),
            seed,
            None,
        )?;

        Ok(Self {
            wallet: Arc::new(wallet),
            wait_invoice_active: Arc::new(AtomicBool::new(false)),
            pending_mints: Arc::new(Mutex::new(FuturesUnordered::new())),
            kagi_auth_token: kagi_auth_token.to_string(),
            cost_per_xsr_cents,
        })
    }
}

#[async_trait]
impl MintPayment for CashuWalletBackend {
    type Err = cdk_common::payment::Error;

    async fn get_settings(&self) -> Result<SettingsResponse, Self::Err> {
        Ok(SettingsResponse {
            unit: XSR_COMMON_UNIT.to_string(),
            bolt11: Some(Bolt11Settings {
                mpp: false,
                amountless: false,
                invoice_description: true,
            }),
            bolt12: None,
            custom: std::collections::HashMap::new(),
        })
    }

    async fn create_incoming_payment_request(
        &self,
        unit: &CommonCurrencyUnit,
        options: IncomingPaymentOptions,
    ) -> Result<CreateIncomingPaymentResponse, Self::Err> {
        println!("got here");
        let unit = unit.clone();
        let amount = match options {
            IncomingPaymentOptions::Bolt11(opts) => Amount::new(opts.amount.to_u64(), unit.clone()),
            _ => return Err(cdk_common::payment::Error::UnsupportedPaymentOption),
        };

        let usd_price = get_usd_price()
            .await
            .map_err(cdk_common::payment::Error::Lightning)?;
        let msats = cents_to_msats(self.cost_per_xsr_cents * amount.clone().to_u64(), usd_price);

        let amount_sats = Amount::new(msats, CurrencyUnit::Msat).convert_to(&CurrencyUnit::Sat)?;

        let quote = self
            .wallet
            .mint_quote(
                PaymentMethod::BOLT11,
                Some(amount_sats.clone().into()),
                None,
                None,
            )
            .await
            .map_err(|e| cdk_common::payment::Error::Lightning(Box::new(e)))?;

        let quote_id = quote.id.clone();
        let quote_id_for_response = quote.id.clone();

        let original_amount = amount.clone();

        let payment_info = IncomingPaymentInfo {
            cost_sats: amount_sats.to_u64(),
            amount_xsr: amount.to_u64(),
        };
        let value = serde_json::to_vec(&payment_info)?;

        self.wallet
            .localstore
            .kv_write(
                KV_PRIMARY_NAMESPACE,
                KV_SECONDARY_NAMESPACE,
                &quote_id,
                &value,
            )
            .await
            .map_err(|e| cdk_common::payment::Error::Lightning(Box::new(e)))?;

        let wallet = self.wallet.clone();

        let expiry = Some(quote.expiry);
        let request = quote.request.clone();

        let handle = tokio::spawn(async move {
            let result = wallet
                .wait_and_mint_quote(
                    quote,
                    Default::default(),
                    Default::default(),
                    Duration::from_secs(500),
                )
                .await;

            match result {
                Ok(_) => Some(WaitPaymentResponse {
                    payment_identifier: PaymentIdentifier::CustomId(quote_id.clone()),
                    payment_amount: CommonAmount::new(original_amount.to_u64(), unit.clone()),
                    payment_id: quote_id,
                }),
                Err(_) => None,
            }
        });

        let pending = self.pending_mints.lock().await;
        pending.push(handle);

        Ok(CreateIncomingPaymentResponse {
            request_lookup_id: PaymentIdentifier::CustomId(quote_id_for_response),
            request,
            expiry,
            extra_json: None,
        })
    }

    async fn get_payment_quote(
        &self,
        _unit: &CommonCurrencyUnit,
        _options: OutgoingPaymentOptions,
    ) -> Result<PaymentQuoteResponse, Self::Err> {
        Ok(PaymentQuoteResponse {
            request_lookup_id: None,
            amount: Amount::new(1, XSR_UNIT.clone()),
            fee: Amount::new(0, XSR_UNIT.clone()),
            state: cdk_common::MeltQuoteState::Unpaid,
        })
    }

    async fn make_payment(
        &self,
        _unit: &CommonCurrencyUnit,
        options: OutgoingPaymentOptions,
    ) -> Result<MakePaymentResponse, Self::Err> {
        match options {
            OutgoingPaymentOptions::Custom(options) => {
                let response = reqwest::Client::new()
                    .get("https://kagi.com/api/v0/search")
                    .header(
                        reqwest::header::AUTHORIZATION,
                        format!("Bot {}", self.kagi_auth_token),
                    )
                    .query(&[("q", options.request)])
                    .send()
                    .await
                    .map_err(|e| cdk_common::payment::Error::Lightning(Box::new(e)))?;

                let json_response = response
                    .json::<Value>()
                    .await
                    .map_err(|e| cdk_common::payment::Error::Lightning(Box::new(e)))?;

                Ok(MakePaymentResponse {
                    payment_lookup_id: PaymentIdentifier::CustomId(Uuid::new_v4().to_string()),
                    payment_proof: Some(json_response.to_string()),
                    status: cdk_common::MeltQuoteState::Paid,
                    total_spent: Amount::new(1, XSR_UNIT.clone()),
                })
            }
            _ => unimplemented!(),
        }
    }

    async fn wait_payment_event(
        &self,
    ) -> Result<Pin<Box<dyn Stream<Item = Event> + Send>>, Self::Err> {
        if let Ok(unissed_quotes) = self.wallet.get_unissued_mint_quotes().await {
            for quote in unissed_quotes {
                let wallet = Arc::clone(&self.wallet);
                let handle = tokio::spawn(async move {
                    let quote_id = quote.id.clone();
                    let result = wallet
                        .wait_and_mint_quote(
                            quote,
                            Default::default(),
                            Default::default(),
                            Duration::from_secs(5),
                        )
                        .await;

                    match result {
                        Ok(_) => {
                            let cost_info = wallet
                                .localstore
                                .kv_read(KV_PRIMARY_NAMESPACE, KV_SECONDARY_NAMESPACE, &quote_id)
                                .await
                                .map_err(|e| cdk_common::payment::Error::from(anyhow::anyhow!(e)))
                                .ok()?
                                .ok_or_else(|| {
                                    cdk_common::payment::Error::from(anyhow::anyhow!(
                                        "Missing payment info"
                                    ))
                                })
                                .ok()?;

                            let cost_info: IncomingPaymentInfo = serde_json::from_slice(&cost_info)
                                .map_err(|e| cdk_common::payment::Error::from(anyhow::anyhow!(e)))
                                .ok()?;

                            Some(WaitPaymentResponse {
                                payment_identifier: PaymentIdentifier::CustomId(quote_id.clone()),
                                payment_amount: CommonAmount::new(
                                    cost_info.amount_xsr,
                                    XSR_COMMON_UNIT.clone(),
                                ),
                                payment_id: quote_id,
                            })
                        }
                        Err(_) => None,
                    }
                });

                let pending = self.pending_mints.lock().await;
                pending.push(handle);
            }
        }

        let pending_mints = Arc::clone(&self.pending_mints);

        let stream = futures::stream::unfold(pending_mints, |pending_mints| async move {
            let mut pending = pending_mints.lock().await;

            if let Some(result) = pending.next().await {
                drop(pending);
                match result {
                    Ok(Some(response)) => {
                        Some((Some(Event::PaymentReceived(response)), pending_mints))
                    }
                    Ok(None) | Err(_) => Some((None, pending_mints)),
                }
            } else {
                drop(pending);
                tokio::time::sleep(Duration::from_millis(100)).await;
                Some((None, pending_mints))
            }
        })
        .filter_map(futures::future::ready);

        Ok(Box::pin(stream))
    }

    fn is_wait_invoice_active(&self) -> bool {
        self.wait_invoice_active.load(Ordering::Relaxed)
    }

    fn cancel_wait_invoice(&self) {
        self.wait_invoice_active.store(false, Ordering::Relaxed);
    }

    async fn check_incoming_payment_status(
        &self,
        payment_identifier: &PaymentIdentifier,
    ) -> Result<Vec<WaitPaymentResponse>, Self::Err> {
        let quote_id = match payment_identifier {
            PaymentIdentifier::CustomId(id) => id,
            _ => return Err(cdk_common::payment::Error::UnsupportedPaymentOption),
        };

        let mint_quote = self
            .wallet
            .check_mint_quote_status(quote_id)
            .await
            .map_err(|e| cdk_common::payment::Error::Lightning(Box::new(e)))?;

        match mint_quote.state {
            cdk::nuts::MintQuoteState::Paid => {
                let _receive_amount = self
                    .wallet
                    .mint(quote_id, SplitTarget::default(), None)
                    .await
                    .map_err(|e| cdk_common::payment::Error::Lightning(Box::new(e)))?;

                let cost_info = self
                    .wallet
                    .localstore
                    .kv_read(KV_PRIMARY_NAMESPACE, KV_SECONDARY_NAMESPACE, quote_id)
                    .await
                    .map_err(|e| cdk_common::payment::Error::from(anyhow::anyhow!(e)))?
                    .ok_or_else(|| {
                        cdk_common::payment::Error::from(anyhow::anyhow!("Missing payment info"))
                    })?;

                let cost_info: IncomingPaymentInfo = serde_json::from_slice(&cost_info)
                    .map_err(|e| cdk_common::payment::Error::from(anyhow::anyhow!(e)))?;

                Ok(vec![WaitPaymentResponse {
                    payment_identifier: payment_identifier.clone(),
                    payment_amount: CommonAmount::new(
                        cost_info.amount_xsr,
                        XSR_COMMON_UNIT.clone(),
                    ),
                    payment_id: quote_id.clone(),
                }])
            }
            _ => Ok(vec![]),
        }
    }

    async fn check_outgoing_payment(
        &self,
        _payment_identifier: &PaymentIdentifier,
    ) -> Result<MakePaymentResponse, Self::Err> {
        todo!("Implement check_outgoing_payment")
    }
}

#[derive(Debug, Deserialize)]
struct PriceResponse {
    #[serde(rename = "USD")]
    usd: u64,
}

async fn get_usd_price() -> Result<u64, Box<dyn std::error::Error + Send + Sync + 'static>> {
    let client = reqwest::Client::new();
    let response = client
        .get("https://mempool.space/api/v1/prices")
        .send()
        .await?
        .json::<PriceResponse>()
        .await?;

    Ok(response.usd)
}

fn cents_to_msats(cents: u64, btc_price_dollars: u64) -> u64 {
    let bitcoin_price_cents = btc_price_dollars * 100;
    let msats = (cents as u128 * 100_000_000_000u128) / bitcoin_price_cents as u128;
    let rounded_sats = msats.div_ceil(1000);
    let rounded_msats = rounded_sats * 1000;

    rounded_msats as u64
}
