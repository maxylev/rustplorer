use crate::format::format_to_human;
use crate::rpc::execute_rpc;
use crate::{AssetConfig, DepositResult};
use futures::StreamExt;
use hashbrown::HashSet;
use num_bigint::BigUint;
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

        // Collect all relevant signatures by watched address, then fetch each
        // transaction. This is the idiomatic Solana deposit-monitoring pattern
        // and works on solana-test-validator, where getBlock may omit user txs.
        //
        // SPL token transfers normally reference the recipient token account,
        // not necessarily the wallet owner. Add configured token accounts for
        // every watched owner so scan_spl_static can still detect owner-level
        // deposits from transaction token-balance metadata.
        let mut signature_query_addresses: HashSet<String> = targets.iter().cloned().collect();
        for owner in targets.iter() {
            for asset in self.assets.values() {
                if asset.contract.eq_ignore_ascii_case("native") {
                    continue;
                }

                let payload = json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "getTokenAccountsByOwner",
                    "params": [
                        owner,
                        { "mint": asset.contract },
                        { "encoding": "jsonParsed", "commitment": "confirmed" }
                    ]
                });

                let response = match execute_rpc(&client, &self.rpc_urls, &payload).await {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!(owner = %owner, mint = %asset.contract, error = %e, "getTokenAccountsByOwner failed");
                        continue;
                    }
                };

                if let Some(accounts) = response["result"]["value"].as_array() {
                    for account in accounts {
                        if let Some(pubkey) = account["pubkey"].as_str() {
                            signature_query_addresses.insert(pubkey.to_string());
                        }
                    }
                }
            }
        }

        let mut all_sigs: Vec<(String, u64)> = Vec::new();

        // Some local validators can report `getSlot == 0` for a short time even
        // after confirmed transactions are visible through
        // `getSignaturesForAddress`. Treat a zero-width Solana range as an
        // open-ended scan from `start` so local/demo scans don't miss fresh
        // deposits merely because the tip RPC lagged behind signature history.
        let effective_end = if end <= start { u64::MAX } else { end };

        for addr in signature_query_addresses.iter() {
            let mut before: Option<String> = None;

            loop {
                let mut params = json!([
                    addr,
                    {
                        "limit": 1000,
                        "minContextSlot": start,
                        "commitment": "confirmed"
                    }
                ]);
                if let Some(ref cursor) = before {
                    params[1]["before"] = json!(cursor);
                }

                let payload = json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "getSignaturesForAddress",
                    "params": params
                });

                let response = match execute_rpc(&client, &self.rpc_urls, &payload).await {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!(addr = %addr, error = %e, "getSignaturesForAddress failed");
                        break;
                    }
                };

                let sigs = match response["result"].as_array() {
                    Some(a) => a,
                    None => break,
                };

                if sigs.is_empty() {
                    break;
                }

                let mut done = false;
                for sig_entry in sigs {
                    let slot = sig_entry["slot"].as_u64().unwrap_or(0);
                    if slot > effective_end {
                        // Results are newest-first. Keep paginating until we
                        // reach signatures inside or below the requested range.
                        continue;
                    }
                    if slot < start {
                        done = true;
                        break;
                    }
                    if let Some(sig) = sig_entry["signature"].as_str() {
                        all_sigs.push((sig.to_string(), slot));
                    }
                }

                if done {
                    break;
                }

                before = sigs
                    .last()
                    .and_then(|e| e["signature"].as_str())
                    .map(ToString::to_string);

                if before.is_none() {
                    break;
                }
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
        }

        // Multiple watched addresses can appear in the same transaction.
        all_sigs.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        all_sigs.dedup_by(|a, b| a.0 == b.0);

        let fetches = futures::stream::iter(all_sigs.into_iter().map(|(sig, slot)| {
            let client = Arc::clone(&client);
            let rpc_urls = self.rpc_urls.clone();
            let caip2 = self.caip2.clone();
            let chain_name = self.name.clone();
            let targets = Arc::clone(&targets);
            let tx = tx.clone();
            let assets = self.assets.clone();

            async move {
                let payload = json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "getTransaction",
                    "params": [
                        sig,
                        {
                            "encoding": "json",
                            "maxSupportedTransactionVersion": 0,
                            "commitment": "confirmed"
                        }
                    ]
                });

                let response = match execute_rpc(&client, &rpc_urls, &payload).await {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!(caip2 = %caip2, sig = %sig, error = %e, "getTransaction failed");
                        return;
                    }
                };

                let txn = &response["result"];
                if txn.is_null() {
                    return;
                }

                let ctx = ScanCtx {
                    slot,
                    name: &chain_name,
                    tx_hash: &sig,
                    targets: &targets,
                    tx: &tx,
                    assets: &assets,
                };
                process_transaction_static(txn, &ctx).await;
            }
        }))
        .buffer_unordered(max_concurrent);

        fetches.collect::<Vec<_>>().await;

        Ok(())
    }
}

struct ScanCtx<'a> {
    slot: u64,
    name: &'a str,
    tx_hash: &'a str,
    targets: &'a Arc<HashSet<String>>,
    tx: &'a mpsc::Sender<DepositResult>,
    assets: &'a HashMap<String, AssetConfig>,
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

            let native_asset_name = ctx
                .assets
                .iter()
                .find_map(|(name, a)| {
                    if a.contract == "native" {
                        Some(name.as_str())
                    } else {
                        None
                    }
                })
                .unwrap_or("Native");

            let _ = ctx
                .tx
                .send(DepositResult {
                    chain: ctx.name.to_string(),
                    asset: native_asset_name.to_string(),
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
        let configured_asset = ctx
            .assets
            .iter()
            .find(|(_, asset)| asset.contract == mint)
            .map(|(name, asset)| (name.as_str(), asset.decimals));

        if ctx
            .assets
            .values()
            .any(|asset| !asset.contract.eq_ignore_ascii_case("native"))
            && configured_asset.is_none()
        {
            continue;
        }

        let configured_decimals = configured_asset
            .map(|(_, decimals)| decimals)
            .unwrap_or_else(|| post_log["uiTokenAmount"]["decimals"].as_u64().unwrap_or(6) as u32);

        let post_amount_str = post_log["uiTokenAmount"]["amount"].as_str().unwrap_or("0");
        let post_amount = post_amount_str.parse::<BigUint>().unwrap_or_default();

        let pre_amount = if let Some(pre_arr) = pre_token {
            pre_arr
                .iter()
                .find(|pre_log| {
                    pre_log["owner"].as_str() == Some(owner)
                        && pre_log["mint"].as_str() == Some(mint)
                })
                .and_then(|pre_log| {
                    pre_log["uiTokenAmount"]["amount"]
                        .as_str()
                        .and_then(|s| s.parse::<BigUint>().ok())
                })
                .unwrap_or_default()
        } else {
            BigUint::default()
        };

        if post_amount <= pre_amount {
            continue;
        }

        let diff = &post_amount - &pre_amount;
        let diff_str = diff.to_string();

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
                asset: configured_asset
                    .map(|(name, _)| name.to_string())
                    .unwrap_or_else(|| mint.to_string()),
                from_address: from_addr,
                to_address: owner.to_string(),
                amount_raw: diff_str.clone(),
                amount_clean: format_to_human(&diff_str, configured_decimals),
                block_number: ctx.slot,
                tx_hash: ctx.tx_hash.to_string(),
            })
            .await;
    }
}
