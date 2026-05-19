use crate::format::format_to_human;
use crate::rpc::execute_rpc;
use crate::{AssetConfig, DepositResult};
use hashbrown::HashSet;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

const TRANSFER_TOPIC: &str = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";
const BLOCK_CHUNK_SIZE: usize = 200;
const NATIVE_DECIMALS: u32 = 18;
const RPC_DELAY_MS: u64 = 100;

pub struct EvmScanner {
    pub rpc_urls: Vec<String>,
    pub caip2: String,
    pub assets: Arc<HashMap<String, AssetConfig>>,
}

impl EvmScanner {
    pub async fn scan(
        &self,
        client: reqwest::Client,
        start: u64,
        end: u64,
        targets: Arc<HashSet<String>>,
        results: Arc<Mutex<Vec<DepositResult>>>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        for current_start in (start..=end).step_by(BLOCK_CHUNK_SIZE) {
            let current_end = std::cmp::min(current_start + BLOCK_CHUNK_SIZE as u64 - 1, end);

            self.scan_erc20(&client, current_start, current_end, &targets, &results)
                .await?;

            self.scan_native(&client, current_start, current_end, &targets, &results)
                .await?;

            tokio::time::sleep(tokio::time::Duration::from_millis(RPC_DELAY_MS)).await;
        }
        Ok(())
    }

    async fn scan_erc20(
        &self,
        client: &reqwest::Client,
        current_start: u64,
        current_end: u64,
        targets: &Arc<HashSet<String>>,
        results: &Arc<Mutex<Vec<DepositResult>>>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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

                                results.lock().await.push(DepositResult {
                                    chain: self.caip2.clone(),
                                    token: asset_name.clone(),
                                    from_address: clean_from,
                                    to_address: clean_to,
                                    amount_raw: raw_amount.to_string(),
                                    amount_clean: format_to_human(raw_amount, asset.decimals),
                                    block_number,
                                });
                            }
                        }
                    }
                }
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        }
        Ok(())
    }

    async fn scan_native(
        &self,
        client: &reqwest::Client,
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

        for block_num in current_start..=current_end {
            let payload = json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "eth_getBlockByNumber",
                "params": [format!("0x{:x}", block_num), true]
            });

            let response = execute_rpc(client, &self.rpc_urls, &payload).await?;

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

                            results.lock().await.push(DepositResult {
                                chain: self.caip2.clone(),
                                token: "Native".to_string(),
                                from_address: clean_from,
                                to_address: clean_to,
                                amount_raw: value.to_string(),
                                amount_clean: format_to_human(value, NATIVE_DECIMALS),
                                block_number: block_num,
                            });
                        }
                    }
                }
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        }
        Ok(())
    }
}

fn extract_address(topic: &Value) -> String {
    let raw = topic.as_str().unwrap_or("");
    if raw.len() == 66 {
        format!("0x{}", &raw[26..]).to_lowercase()
    } else {
        raw.to_lowercase()
    }
}

fn parse_hex_block(val: &Value) -> u64 {
    val.as_str()
        .map(|s| {
            u64::from_str_radix(s.trim_start_matches("0x").trim_start_matches("0X"), 16)
                .unwrap_or(0)
        })
        .unwrap_or(0)
}
