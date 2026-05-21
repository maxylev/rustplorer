use crate::format::format_to_human;
use crate::rpc::execute_rpc;
use crate::{AssetConfig, DepositResult};
use futures::StreamExt;
use hashbrown::HashSet;
use rust_decimal::prelude::*;
use serde_json::json;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::mpsc;

const BTC_DECIMALS: u32 = 8;
const DEFAULT_RPC_DELAY_MS: u64 = 100;
const DEFAULT_MAX_CONCURRENT: usize = 3;

pub struct BtcScanner {
    pub rpc_urls: Vec<String>,
    pub caip2: String,
    pub name: String,
    /// Chain-local assets — already scoped to this chain, no caip2 filter needed.
    pub assets: HashMap<String, AssetConfig>,
    pub rpc_delay_ms: Option<u64>,
    pub max_concurrent: usize,
}

impl BtcScanner {
    /// Get the current block count from the Bitcoin RPC node.
    pub async fn get_tip(
        client: &reqwest::Client,
        rpc_urls: &[String],
    ) -> Result<u64, anyhow::Error> {
        let payload = json!({
            "jsonrpc": "1.0",
            "id": "rustplorer",
            "method": "getblockcount",
            "params": []
        });
        let res = execute_rpc(client, rpc_urls, &payload).await?;

        if let Some(error) = res.get("error")
            && !error.is_null()
        {
            let msg = error["message"].as_str().unwrap_or("Unknown RPC error");
            anyhow::bail!("RPC error in getblockcount: {}", msg);
        }

        res["result"]
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("Failed to parse block count from RPC response"))
    }

    pub async fn scan(
        &self,
        client: reqwest::Client,
        start: u64,
        end: u64,
        targets: Arc<HashSet<String>>,
        tx: mpsc::Sender<DepositResult>,
    ) -> Result<(), anyhow::Error> {
        let client = Arc::new(client);
        let delay_ms = self.rpc_delay_ms.unwrap_or(DEFAULT_RPC_DELAY_MS);
        let max_concurrent = if self.max_concurrent > 0 {
            self.max_concurrent
        } else {
            DEFAULT_MAX_CONCURRENT
        };

        let block_range: Vec<u64> = (start..=end).collect();

        let native_asset_name = self
            .assets
            .iter()
            .find_map(|(name, a)| {
                if a.contract == "native" {
                    Some(name.clone())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "Native".to_string());

        let fetches = futures::stream::iter(block_range.into_iter().map(|block_num| {
            let client = Arc::clone(&client);
            let rpc_urls = self.rpc_urls.clone();
            let caip2 = self.caip2.clone();
            let chain_name = self.name.clone();
            let targets = Arc::clone(&targets);
            let tx = tx.clone();
            let native_asset_name = native_asset_name.clone();
            async move {
                let hash_payload = json!({
                    "jsonrpc": "1.0",
                    "id": "rustplorer",
                    "method": "getblockhash",
                    "params": [block_num]
                });

                let hash_res = match execute_rpc(&client, &rpc_urls, &hash_payload).await {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::error!(
                            caip2 = %caip2,
                            block = block_num,
                            error = %e,
                            "Failed to fetch block hash"
                        );
                        return;
                    }
                };
                let block_hash = match hash_res["result"].as_str() {
                    Some(h) => h,
                    None => return,
                };

                let block_payload = json!({
                    "jsonrpc": "1.0",
                    "id": "rustplorer",
                    "method": "getblock",
                    "params": [block_hash, 3]
                });

                let block_res = match execute_rpc(&client, &rpc_urls, &block_payload).await {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::error!(
                            caip2 = %caip2,
                            block = block_num,
                            error = %e,
                            "Failed to fetch block data"
                        );
                        return;
                    }
                };

                if let Some(transactions) = block_res["result"]["tx"].as_array() {
                    for txn in transactions {
                        process_transaction_static(
                            txn,
                            block_num,
                            &caip2,
                            &chain_name,
                            &targets,
                            &tx,
                            &native_asset_name,
                        )
                        .await;
                    }
                }
            }
        }))
        .buffer_unordered(max_concurrent);

        fetches.collect::<Vec<_>>().await;

        tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;

        Ok(())
    }
}

async fn process_transaction_static(
    txn: &serde_json::Value,
    block_num: u64,
    _caip2: &str,
    name: &str,
    targets: &Arc<HashSet<String>>,
    tx: &mpsc::Sender<DepositResult>,
    asset_name: &str,
) {
    let vouts = match txn["vout"].as_array() {
        Some(v) => v,
        None => return,
    };

    let tx_hash = txn["txid"].as_str().unwrap_or("unknown").to_string();

    let mut from_address = "unknown".to_string();
    if let Some(vins) = txn["vin"].as_array()
        && let Some(first_vin) = vins.first()
        && let Some(prevout) = first_vin.get("prevout")
        && let Some(addr) = extract_btc_address(prevout)
    {
        from_address = addr;
    }

    for vout in vouts {
        if let Some(to_address) = extract_btc_address(vout)
            && targets.contains(&to_address)
        {
            // 2026 Best Practice: Extract the exact string representation from
            // serde_json (with arbitrary_precision enabled), completely bypassing
            // IEEE-754 floating-point math. This prevents precision loss for
            // high-value Bitcoin transactions.
            let exact_val_str = vout["value"]
                .as_number()
                .map(|n| n.to_string())
                .unwrap_or_else(|| "0".to_string());

            let decimal_val = Decimal::from_str(&exact_val_str).unwrap_or_default();
            let sats_decimal = decimal_val * Decimal::new(100_000_000, 0);
            let raw_amount = sats_decimal.trunc().to_string();

            let _ = tx
                .send(DepositResult {
                    chain: name.to_string(),
                    asset: asset_name.to_string(),
                    from_address: from_address.clone(),
                    to_address,
                    amount_raw: raw_amount.clone(),
                    amount_clean: format_to_human(&raw_amount, BTC_DECIMALS),
                    block_number: block_num,
                    tx_hash: tx_hash.clone(),
                })
                .await;
        }
    }
}

fn extract_btc_address(out: &serde_json::Value) -> Option<String> {
    let spk = out.get("scriptPubKey")?;
    if let Some(addr) = spk.get("address").and_then(|a| a.as_str()) {
        return Some(addr.to_string());
    }
    if let Some(addrs) = spk.get("addresses").and_then(|a| a.as_array())
        && let Some(addr) = addrs.first().and_then(|a| a.as_str())
    {
        return Some(addr.to_string());
    }
    None
}
