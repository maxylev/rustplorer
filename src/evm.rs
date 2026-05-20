use crate::format::format_to_human;
use crate::rpc::execute_rpc;
use crate::{AssetConfig, DepositResult};
use futures::StreamExt;
use hashbrown::HashSet;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

const TRANSFER_TOPIC: &str = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";
const BLOCK_CHUNK_SIZE: usize = 200;
const NATIVE_DECIMALS: u32 = 18;
const DEFAULT_RPC_DELAY_MS: u64 = 100;
const DEFAULT_MAX_CONCURRENT: usize = 5;
const ADDRESS_PADDING: &str = "000000000000000000000000";

pub struct EvmScanner {
    pub rpc_urls: Vec<String>,
    pub caip2: String,
    pub assets: Arc<HashMap<String, AssetConfig>>,
    pub rpc_delay_ms: Option<u64>,
    pub max_concurrent: usize,
}

impl EvmScanner {
    pub async fn get_tip(
        client: &reqwest::Client,
        rpc_urls: &[String],
    ) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
        let payload = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_blockNumber",
            "params": []
        });
        let res = execute_rpc(client, rpc_urls, &payload).await?;
        let hex = res["result"]
            .as_str()
            .unwrap_or("0x0")
            .trim_start_matches("0x");
        Ok(u64::from_str_radix(hex, 16).unwrap_or(0))
    }

    pub async fn scan(
        &self,
        client: reqwest::Client,
        start: u64,
        end: u64,
        targets: Arc<HashSet<String>>,
        results: Arc<Mutex<Vec<DepositResult>>>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let delay_ms = self.rpc_delay_ms.unwrap_or(DEFAULT_RPC_DELAY_MS);
        let client = Arc::new(client);

        for current_start in (start..=end).step_by(BLOCK_CHUNK_SIZE) {
            let current_end = std::cmp::min(current_start + BLOCK_CHUNK_SIZE as u64 - 1, end);

            self.scan_erc20(&client, current_start, current_end, &targets, &results)
                .await?;

            self.scan_native(&client, current_start, current_end, &targets, &results)
                .await?;

            tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
        }
        Ok(())
    }

    async fn scan_erc20(
        &self,
        client: &Arc<reqwest::Client>,
        current_start: u64,
        current_end: u64,
        targets: &Arc<HashSet<String>>,
        results: &Arc<Mutex<Vec<DepositResult>>>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let delay_ms = self.rpc_delay_ms.unwrap_or(DEFAULT_RPC_DELAY_MS);

        for (asset_name, asset) in self
            .assets
            .iter()
            .filter(|(_, a)| a.network == self.caip2 && a.contract != "native")
        {
            let payload = json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "eth_getLogs",
                "params": [{
                    "address": asset.contract,
                    "fromBlock": format!("0x{:x}", current_start),
                    "toBlock": format!("0x{:x}", current_end),
                    "topics": [TRANSFER_TOPIC]
                }]
            });

            let response = execute_rpc(client, &self.rpc_urls, &payload).await?;

            if let Some(logs) = response["result"].as_array() {
                for log in logs {
                    if let Some(topics) = log["topics"].as_array() {
                        if topics.len() >= 3 {
                            let clean_from = extract_address(&topics[1]);
                            let clean_to = extract_address(&topics[2]);

                            if targets.contains(&clean_to) {
                                let raw_amount = log["data"].as_str().unwrap_or("0x0");
                                let block_number = parse_hex_block(&log["blockNumber"]);
                                let tx_hash =
                                    log["transactionHash"].as_str().unwrap_or("").to_string();

                                results.lock().await.push(DepositResult {
                                    chain: self.caip2.clone(),
                                    token: asset_name.clone(),
                                    from_address: clean_from,
                                    to_address: clean_to,
                                    amount_raw: raw_amount.to_string(),
                                    amount_clean: format_to_human(raw_amount, asset.decimals),
                                    block_number,
                                    tx_hash,
                                });
                            }
                        }
                    }
                }
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms / 2)).await;
        }
        Ok(())
    }

    async fn scan_native(
        &self,
        client: &Arc<reqwest::Client>,
        current_start: u64,
        current_end: u64,
        targets: &Arc<HashSet<String>>,
        results: &Arc<Mutex<Vec<DepositResult>>>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let has_native = self
            .assets
            .iter()
            .any(|(_, a)| a.network == self.caip2 && a.contract == "native");

        if !has_native {
            return Ok(());
        }

        let max_concurrent = if self.max_concurrent > 0 {
            self.max_concurrent
        } else {
            DEFAULT_MAX_CONCURRENT
        };

        let block_range: Vec<u64> = (current_start..=current_end).collect();
        let fetches = futures::stream::iter(block_range.into_iter().map(|block_num| {
            let client = Arc::clone(client);
            let rpc_urls = self.rpc_urls.clone();
            let caip2 = self.caip2.clone();
            let targets = Arc::clone(targets);
            let results = Arc::clone(results);
            async move {
                let payload = json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "eth_getBlockByNumber",
                    "params": [format!("0x{:x}", block_num), true]
                });

                match execute_rpc(&client, &rpc_urls, &payload).await {
                    Ok(response) => {
                        if let Some(transactions) = response["result"]["transactions"].as_array() {
                            for tx in transactions {
                                let value = tx["value"].as_str().unwrap_or("0x0");
                                if value == "0x0" || value == "0x" {
                                    continue;
                                }

                                if let Some(to_addr) = tx["to"].as_str() {
                                    let clean_to = to_addr.to_lowercase();
                                    if targets.contains(&clean_to) {
                                        let clean_from = tx["from"]
                                            .as_str()
                                            .unwrap_or("0x0000000000000000000000000000000000000000")
                                            .to_lowercase();
                                        let tx_hash = tx["hash"].as_str().unwrap_or("").to_string();

                                        results.lock().await.push(DepositResult {
                                            chain: caip2.clone(),
                                            token: "Native".to_string(),
                                            from_address: clean_from,
                                            to_address: clean_to,
                                            amount_raw: value.to_string(),
                                            amount_clean: format_to_human(value, NATIVE_DECIMALS),
                                            block_number: block_num,
                                            tx_hash,
                                        });
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("[rustplorer] [{}] block {}: {}", caip2, block_num, e);
                    }
                }
            }
        }))
        .buffer_unordered(max_concurrent);

        fetches.collect::<Vec<_>>().await;

        Ok(())
    }
}

fn extract_address(topic: &Value) -> String {
    let raw = topic.as_str().unwrap_or("");
    if raw.len() == 66 && raw.starts_with("0x") {
        let lower_prefix = &raw[2..26];
        if lower_prefix == ADDRESS_PADDING {
            return format!("0x{}", &raw[26..]).to_lowercase();
        }
    }
    raw.to_lowercase()
}

fn parse_hex_block(val: &Value) -> u64 {
    val.as_str()
        .map(|s| {
            u64::from_str_radix(s.trim_start_matches("0x").trim_start_matches("0X"), 16)
                .unwrap_or(0)
        })
        .unwrap_or(0)
}
