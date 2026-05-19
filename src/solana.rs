use crate::format::format_to_human;
use crate::rpc::execute_rpc;
use crate::{AssetConfig, DepositResult};
use hashbrown::HashSet;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

const SOL_DECIMALS: u32 = 9;
const RPC_DELAY_MS: u64 = 200;

pub struct SolanaScanner {
    pub rpc_urls: Vec<String>,
    pub caip2: String,
    pub assets: Arc<HashMap<String, AssetConfig>>,
}

impl SolanaScanner {
    pub async fn scan(
        &self,
        client: reqwest::Client,
        start: u64,
        end: u64,
        targets: Arc<HashSet<String>>,
        results: Arc<Mutex<Vec<DepositResult>>>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        for slot in start..=end {
            let payload = json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "getBlock",
                "params": [
                    slot,
                    {
                        "encoding": "json",
                        "transactionDetails": "full",
                        "rewards": false,
                        "maxSupportedTransactionVersion": 0
                    }
                ]
            });

            let response = match execute_rpc(&client, &self.rpc_urls, &payload).await {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("[rustplorer] [{}] slot {}: {}", self.caip2, slot, e);
                    tokio::time::sleep(tokio::time::Duration::from_millis(RPC_DELAY_MS)).await;
                    continue;
                }
            };

            if let Some(transactions) = response["result"]["transactions"].as_array() {
                for tx in transactions {
                    self.process_transaction(tx, slot, &targets, &results).await;
                }
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(RPC_DELAY_MS)).await;
        }
        Ok(())
    }

    async fn process_transaction(
        &self,
        tx: &serde_json::Value,
        slot: u64,
        targets: &Arc<HashSet<String>>,
        results: &Arc<Mutex<Vec<DepositResult>>>,
    ) {
        let account_keys = match tx["transaction"]["message"]["accountKeys"].as_array() {
            Some(keys) => keys,
            None => return,
        };
        let meta = match tx["meta"].as_object() {
            Some(m) => m,
            None => return,
        };

        let sender = account_keys
            .first()
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        self.scan_native_balances(account_keys, meta, sender, slot, targets, results)
            .await;

        self.scan_spl_balances(tx, meta, slot, targets, results)
            .await;
    }

    async fn scan_native_balances(
        &self,
        account_keys: &[serde_json::Value],
        meta: &serde_json::Map<String, serde_json::Value>,
        sender: &str,
        slot: u64,
        targets: &Arc<HashSet<String>>,
        results: &Arc<Mutex<Vec<DepositResult>>>,
    ) {
        let pre_arr = match meta.get("preBalances").and_then(|v| v.as_array()) {
            Some(a) => a,
            None => return,
        };
        let post_arr = match meta.get("postBalances").and_then(|v| v.as_array()) {
            Some(a) => a,
            None => return,
        };

        for (index, account_val) in account_keys.iter().enumerate() {
            let addr = match account_val.as_str() {
                Some(a) => a,
                None => continue,
            };

            if !targets.contains(addr) {
                continue;
            }

            let pre_bal = pre_arr.get(index).and_then(|v| v.as_u64()).unwrap_or(0);
            let post_bal = post_arr.get(index).and_then(|v| v.as_u64()).unwrap_or(0);

            if post_bal > pre_bal {
                let diff = post_bal - pre_bal;
                let diff_str = diff.to_string();

                results.lock().await.push(DepositResult {
                    chain: self.caip2.clone(),
                    token: "Native".to_string(),
                    from_address: sender.to_string(),
                    to_address: addr.to_string(),
                    amount_raw: diff_str.clone(),
                    amount_clean: format_to_human(&diff_str, SOL_DECIMALS),
                    block_number: slot,
                });
            }
        }
    }

    async fn scan_spl_balances(
        &self,
        _tx: &serde_json::Value,
        meta: &serde_json::Map<String, serde_json::Value>,
        slot: u64,
        targets: &Arc<HashSet<String>>,
        results: &Arc<Mutex<Vec<DepositResult>>>,
    ) {
        let post_token = match meta.get("postTokenBalances").and_then(|v| v.as_array()) {
            Some(a) => a,
            None => return,
        };
        let pre_token = meta.get("preTokenBalances").and_then(|v| v.as_array());

        for post_log in post_token {
            let owner = match post_log["owner"].as_str() {
                Some(o) => o,
                None => continue,
            };

            if !targets.contains(owner) {
                continue;
            }

            let mint = post_log["mint"].as_str().unwrap_or("unknown");

            let configured_decimals = self
                .assets
                .values()
                .find(|a| a.contract == mint)
                .map(|a| a.decimals)
                .unwrap_or_else(|| {
                    post_log["uiTokenAmount"]["decimals"].as_u64().unwrap_or(6) as u32
                });

            let raw_amount = post_log["uiTokenAmount"]["amount"].as_str().unwrap_or("0");

            let mut from_addr = "unknown".to_string();
            if let Some(pre_arr) = pre_token {
                for pre_log in pre_arr {
                    if pre_log["mint"].as_str().unwrap_or("") == mint {
                        if let Some(pre_owner) = pre_log["owner"].as_str() {
                            if pre_owner != owner {
                                from_addr = pre_owner.to_string();
                                break;
                            }
                        }
                    }
                }
            }

            results.lock().await.push(DepositResult {
                chain: self.caip2.clone(),
                token: mint.to_string(),
                from_address: from_addr,
                to_address: owner.to_string(),
                amount_raw: raw_amount.to_string(),
                amount_clean: format_to_human(raw_amount, configured_decimals),
                block_number: slot,
            });
        }
    }
}
