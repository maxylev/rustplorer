use crate::format::format_to_human;
use crate::rpc::execute_rpc;
use crate::{AssetConfig, DepositResult};
use futures::StreamExt;
use hashbrown::HashSet;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

const SOL_DECIMALS: u32 = 9;
const DEFAULT_RPC_DELAY_MS: u64 = 200;
const DEFAULT_MAX_CONCURRENT: usize = 1;

pub struct SolanaScanner {
    pub rpc_urls: Vec<String>,
    pub caip2: String,
    pub name: String,
    /// Chain-local assets — already scoped to this chain, no caip2 filter needed.
    pub assets: HashMap<String, AssetConfig>,
    pub rpc_delay_ms: Option<u64>,
    pub max_concurrent: usize,
}

impl SolanaScanner {
    /// Get the current slot tip from the Solana RPC node.
    pub async fn get_tip(
        client: &reqwest::Client,
        rpc_urls: &[String],
    ) -> Result<u64, anyhow::Error> {
        let payload = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getSlot",
            "params": []
        });
        let res = execute_rpc(client, rpc_urls, &payload).await?;

        if let Some(error) = res.get("error")
            && !error.is_null()
        {
            let msg = error["message"].as_str().unwrap_or("Unknown RPC error");
            anyhow::bail!("RPC error in getSlot: {}", msg);
        }

        res["result"]
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("Failed to parse slot number from RPC response"))
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

        let slot_range: Vec<u64> = (start..=end).collect();
        let fetches = futures::stream::iter(slot_range.into_iter().map(|slot| {
            let client = Arc::clone(&client);
            let rpc_urls = self.rpc_urls.clone();
            let caip2 = self.caip2.clone();
            let chain_name = self.name.clone();
            let targets = Arc::clone(&targets);
            let tx = tx.clone();
            async move {
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

                let response = match execute_rpc(&client, &rpc_urls, &payload).await {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!(caip2 = %caip2, slot = slot, error = %e, "Failed to fetch slot");
                        return;
                    }
                };

                if let Some(transactions) = response["result"]["transactions"].as_array() {
                    for txn in transactions {
                        let tx_hash = txn["transaction"]["signatures"]
                            .as_array()
                            .and_then(|sig| sig.first())
                            .and_then(|s| s.as_str())
                            .unwrap_or("unknown")
                            .to_string();
                        let ctx = ScanCtx {
                            slot,
                            name: &chain_name,
                            tx_hash: &tx_hash,
                            targets: &targets,
                            tx: &tx,
                        };
                        process_transaction_static(txn, &ctx).await;
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

struct ScanCtx<'a> {
    slot: u64,
    name: &'a str,
    tx_hash: &'a str,
    targets: &'a Arc<HashSet<String>>,
    tx: &'a mpsc::Sender<DepositResult>,
}

async fn process_transaction_static(txn: &serde_json::Value, ctx: &ScanCtx<'_>) {
    let account_keys = match txn["transaction"]["message"]["accountKeys"].as_array() {
        Some(keys) => keys,
        None => return,
    };
    let meta = match txn["meta"].as_object() {
        Some(m) => m,
        None => return,
    };

    let sender = account_keys
        .first()
        .and_then(account_key_to_string)
        .unwrap_or_else(|| "unknown".to_string());

    scan_native_static(account_keys, meta, &sender, ctx).await;
    scan_spl_static(txn, meta, ctx).await;
}

async fn scan_native_static(
    account_keys: &[serde_json::Value],
    meta: &serde_json::Map<String, serde_json::Value>,
    sender: &str,
    ctx: &ScanCtx<'_>,
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
        let addr = match account_key_to_string(account_val) {
            Some(a) => a,
            None => continue,
        };

        if !ctx.targets.contains(&addr) {
            continue;
        }

        let pre_bal = pre_arr.get(index).and_then(|v| v.as_u64()).unwrap_or(0);
        let post_bal = post_arr.get(index).and_then(|v| v.as_u64()).unwrap_or(0);

        if post_bal > pre_bal {
            let diff = post_bal - pre_bal;
            let diff_str = diff.to_string();

            let _ = ctx
                .tx
                .send(DepositResult {
                    chain: ctx.name.to_string(),
                    asset: "Native".to_string(),
                    from_address: sender.to_string(),
                    to_address: addr,
                    amount_raw: diff_str.clone(),
                    amount_clean: format_to_human(&diff_str, SOL_DECIMALS),
                    block_number: ctx.slot,
                    tx_hash: ctx.tx_hash.to_string(),
                })
                .await;
        }
    }
}

fn account_key_to_string(value: &serde_json::Value) -> Option<String> {
    if let Some(s) = value.as_str() {
        return Some(s.to_string());
    }

    value
        .get("pubkey")
        .and_then(|pubkey| pubkey.as_str())
        .map(ToString::to_string)
}

async fn scan_spl_static(
    _txn: &serde_json::Value,
    meta: &serde_json::Map<String, serde_json::Value>,
    ctx: &ScanCtx<'_>,
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

        if !ctx.targets.contains(owner) {
            continue;
        }

        let mint = post_log["mint"].as_str().unwrap_or("unknown");

        let configured_decimals =
            post_log["uiTokenAmount"]["decimals"].as_u64().unwrap_or(6) as u32;

        let raw_amount = post_log["uiTokenAmount"]["amount"].as_str().unwrap_or("0");

        let mut from_addr = "unknown".to_string();
        if let Some(pre_arr) = pre_token {
            for pre_log in pre_arr {
                if pre_log["mint"].as_str().unwrap_or("") == mint
                    && let Some(pre_owner) = pre_log["owner"].as_str()
                    && pre_owner != owner
                {
                    from_addr = pre_owner.to_string();
                    break;
                }
            }
        }

        let _ = ctx
            .tx
            .send(DepositResult {
                chain: ctx.name.to_string(),
                asset: mint.to_string(),
                from_address: from_addr,
                to_address: owner.to_string(),
                amount_raw: raw_amount.to_string(),
                amount_clean: format_to_human(raw_amount, configured_decimals),
                block_number: ctx.slot,
                tx_hash: ctx.tx_hash.to_string(),
            })
            .await;
    }
}
