use std::path::PathBuf;
use std::str::FromStr;
use std::sync::LazyLock;

use anyhow::{anyhow, Result};
use cdk::nuts::CurrencyUnit;
use cdk_common::nuts::CurrencyUnit as CommonCurrencyUnit;

pub mod cdk_wallet;
pub mod cli;
pub mod config;
pub mod search_route_handlers;

pub static XSR_UNIT: LazyLock<CurrencyUnit> =
    LazyLock::new(|| CurrencyUnit::from_str("xsr").expect("xsr is a valid unit"));

pub static XSR_COMMON_UNIT: LazyLock<CommonCurrencyUnit> =
    LazyLock::new(|| CommonCurrencyUnit::Custom("xsr".to_string()));

pub fn work_dir() -> Result<PathBuf> {
    let home_dir = home::home_dir().ok_or(anyhow!("Unknown home dir"))?;

    Ok(home_dir.join(".athenut-mint"))
}

pub fn expand_path(path: &str) -> Option<PathBuf> {
    if path.starts_with('~') {
        if let Some(home_dir) = home::home_dir().as_mut() {
            let remainder = &path[2..];
            home_dir.push(remainder);
            let expanded_path = home_dir;
            Some(expanded_path.clone())
        } else {
            None
        }
    } else {
        Some(PathBuf::from(path))
    }
}
