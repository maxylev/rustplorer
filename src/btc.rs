use crate::format::format_to_human;
use crate::rpc::execute_rpc;
use crate::{AssetConfig, DepositResult};
use hashbrown::HashSet;
use rust_decimal::prelude::*;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

const BTC_DECIMALS: u32 = 8;
const RPC_DELAY_MS: u64 = 100;

pub struct BtcScanner {
    pub rpc_urls: Vec<String>,
    pub caip2: String,
    pub assets: Arc<HashMap<String, AssetConfig>>,
}

impl BtcScanner {
    pub async fn scan(
        &self,
        client: reqwest::Client,
        start: u64,
        end: u64,
        targets: Arc<HashSet<String>>,
        results: Arc<Mutex<Vec<DepositResult>>>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        for block_num in start..=end {
            let hash_payload = json!({
                "jsonrpc": "1.0",
                "id": "rustplorer",
                "method": "getblockhash",
                "params": [block_num]
            });

            let hash_res = execute_rpc(&client, &self.rpc_urls, &hash_payload).await?;
            let block_hash = match hash_res["result"].as_str() {
                Some(h) => h,
                None => continue,
            };

            let block_payload = json!({
                "jsonrpc": "1.0",
                "id": "rustplorer",
                "method": "getblock",
                "params": [block_hash, 3]
            });

            let block_res = execute_rpc(&client, &self.rpc_urls, &block_payload).await?;

            if let Some(transactions) = block_res["result"]["tx"].as_array() {
                for tx in transactions {
                    self.process_transaction(tx, block_num, &targets, &results).await;
                }
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(RPC_DELAY_MS)).await;
        }
        Ok(())
    }

    async fn process_transaction(
        &self,
        tx: &serde_json::Value,
        block_num: u64,
        targets: &Arc<HashSet<String>>,
        results: &Arc<Mutex<Vec<DepositResult>>>,
    ) {
        let vouts = match tx["vout"].as_array() {
            Some(v) => v,
            None => return,
        };

        let mut from_address = "unknown".to_string();
        if let Some(vins) = tx["vin"].as_array() {
            if let Some(first_vin) = vins.first() {
                if let Some(prevout) = first_vin.get("prevout") {
                    if let Some(addr) = extract_btc_address(prevout) {
                        from_address = addr;
                    }
                }
            }
        }

        for vout in vouts {
            if let Some(to_address) = extract_btc_address(vout) {
                if targets.contains(&to_address) {
                    let btc_val_f64 = vout["value"].as_f64().unwrap_or(0.0);

                    let decimal_val = Decimal::from_f64(btc_val_f64).unwrap_or_default();
                    let sats_decimal = decimal_val * Decimal::new(100_000_000, 0);
                    let raw_amount = sats_decimal.trunc().to_string();

                    results.lock().await.push(DepositResult {
                        chain: self.caip2.clone(),
                        token: "Native".to_string(),
                        from_address: from_address.clone(),
                        to_address,
                        amount_raw: raw_amount.clone(),
                        amount_clean: format_to_human(&raw_amount, BTC_DECIMALS),
                        block_number: block_num,
                    });
                }
            }
        }
    }
}

fn extract_btc_address(out: &serde_json::Value) -> Option<String> {
    let spk = out.get("scriptPubKey")?;
    if let Some(addr) = spk.get("address").and_then(|a| a.as_str()) {
        return Some(addr.to_string());
    }
    if let Some(addrs) = spk.get("addresses").and_then(|a| a.as_array()) {
        if let Some(addr) = addrs.first().and_then(|a| a.as_str()) {
            return Some(addr.to_string());
        }
    }
    None
}
