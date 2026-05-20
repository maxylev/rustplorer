pub mod btc;
pub mod evm;
pub mod format;
pub mod rpc;
pub mod solana;

use hashbrown::HashSet;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

pub use format::format_to_human;

pub const DEFAULT_EVM_LOOKBACK: u64 = 1_000;
pub const DEFAULT_SOLANA_LOOKBACK: u64 = 500;
pub const DEFAULT_BTC_LOOKBACK: u64 = 6;

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ChainConfig {
    pub caip2: String,
    pub rpc: Vec<String>,
    pub start_block: Option<u64>,
    pub end_block: Option<u64>,
    #[serde(default)]
    pub rpc_delay_ms: Option<u64>,
    #[serde(default)]
    pub max_concurrent: Option<usize>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AssetConfig {
    pub network: String,
    pub contract: String,
    pub decimals: u32,
}

#[derive(Debug, Deserialize)]
pub struct AppConfig {
    pub chains: Vec<ChainConfig>,
    pub assets: HashMap<String, AssetConfig>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DepositResult {
    pub chain: String,
    pub token: String,
    pub from_address: String,
    pub to_address: String,
    pub amount_raw: String,
    pub amount_clean: String,
    pub block_number: u64,
    pub tx_hash: String,
}

#[derive(Debug, Clone)]
pub struct IndexerResult {
    pub deposits: Vec<DepositResult>,
    pub latest_blocks: HashMap<String, u64>,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Json,
    Csv,
}

pub fn load_config(
    path: &std::path::Path,
) -> Result<AppConfig, Box<dyn std::error::Error + Send + Sync>> {
    let config_str = std::fs::read_to_string(path)?;
    let config: AppConfig = toml::from_str(&config_str)?;
    Ok(config)
}

pub fn load_addresses(
    path: &std::path::Path,
) -> Result<HashSet<String>, Box<dyn std::error::Error + Send + Sync>> {
    let file = std::fs::File::open(path)?;
    let mut set = HashSet::with_capacity(1_000_000);
    use std::io::{BufRead, BufReader};
    for line in BufReader::new(file).lines() {
        let addr = line?.trim().to_string();
        if !addr.is_empty() {
            if addr.starts_with("0x") || addr.starts_with("0X") {
                set.insert(addr.to_lowercase());
            } else {
                set.insert(addr);
            }
        }
    }
    Ok(set)
}

fn default_lookback(caip2: &str) -> u64 {
    if caip2.starts_with("solana:") {
        DEFAULT_SOLANA_LOOKBACK
    } else if caip2.starts_with("bip122:") {
        DEFAULT_BTC_LOOKBACK
    } else {
        DEFAULT_EVM_LOOKBACK
    }
}

pub async fn run_indexer(
    chains: Vec<ChainConfig>,
    assets: HashMap<String, AssetConfig>,
    targets: Arc<HashSet<String>>,
) -> Result<IndexerResult, Box<dyn std::error::Error + Send + Sync>> {
    let detected_deposits: Arc<Mutex<Vec<DepositResult>>> = Arc::new(Mutex::new(Vec::new()));
    let latest_blocks: Arc<Mutex<HashMap<String, u64>>> = Arc::new(Mutex::new(HashMap::new()));
    let client = reqwest::Client::new();
    let shared_assets = Arc::new(assets);
    let mut tasks = vec![];

    for chain in chains {
        if chain.rpc.is_empty() {
            continue;
        }

        let targets_clone = Arc::clone(&targets);
        let results_clone = Arc::clone(&detected_deposits);
        let blocks_clone = Arc::clone(&latest_blocks);
        let assets_map = Arc::clone(&shared_assets);
        let client_clone = client.clone();
        let rpc_clone = chain.rpc.clone();
        let caip2 = chain.caip2.clone();
        let lookback = default_lookback(&caip2);

        let task = tokio::spawn(async move {
            let is_evm = caip2.starts_with("eip155:");
            let is_solana = caip2.starts_with("solana:");
            let is_btc = caip2.starts_with("bip122:");

            let needs_tip = chain.start_block.is_none() || chain.end_block.is_none();

            let current_tip = if needs_tip {
                if is_evm {
                    evm::EvmScanner::get_tip(&client_clone, &rpc_clone)
                        .await
                        .ok()
                } else if is_solana {
                    solana::SolanaScanner::get_tip(&client_clone, &rpc_clone)
                        .await
                        .ok()
                } else if is_btc {
                    btc::BtcScanner::get_tip(&client_clone, &rpc_clone)
                        .await
                        .ok()
                } else {
                    None
                }
            } else {
                None
            };

            let tip = current_tip.unwrap_or(0);

            let end_block = chain.end_block.unwrap_or(tip);

            let start_block = match (chain.start_block, chain.end_block) {
                (Some(s), _) => s,
                (None, Some(_)) => end_block.saturating_sub(lookback),
                (None, None) => tip.saturating_sub(lookback),
            };

            blocks_clone.lock().await.insert(caip2.clone(), end_block);

            if start_block > end_block {
                eprintln!(
                    "[rustplorer] [{}] start_block ({}) > end_block ({}), skipping",
                    caip2, start_block, end_block
                );
                return;
            }

            eprintln!(
                "[rustplorer] [{}] scanning blocks {} → {}",
                caip2, start_block, end_block
            );

            let rpc_delay = chain.rpc_delay_ms;
            let max_concurrent = chain.max_concurrent.unwrap_or(5);

            if is_evm {
                let scanner = evm::EvmScanner {
                    rpc_urls: rpc_clone,
                    caip2,
                    assets: assets_map,
                    rpc_delay_ms: rpc_delay,
                    max_concurrent,
                };
                let _ = scanner
                    .scan(
                        client_clone,
                        start_block,
                        end_block,
                        targets_clone,
                        results_clone,
                    )
                    .await;
            } else if is_solana {
                let scanner = solana::SolanaScanner {
                    rpc_urls: rpc_clone,
                    caip2,
                    assets: assets_map,
                    rpc_delay_ms: rpc_delay,
                    max_concurrent,
                };
                let _ = scanner
                    .scan(
                        client_clone,
                        start_block,
                        end_block,
                        targets_clone,
                        results_clone,
                    )
                    .await;
            } else if is_btc {
                let scanner = btc::BtcScanner {
                    rpc_urls: rpc_clone,
                    caip2,
                    assets: assets_map,
                    rpc_delay_ms: rpc_delay,
                    max_concurrent,
                };
                let _ = scanner
                    .scan(
                        client_clone,
                        start_block,
                        end_block,
                        targets_clone,
                        results_clone,
                    )
                    .await;
            } else {
                eprintln!("[rustplorer] Unsupported network: {}", caip2);
            }
        });
        tasks.push(task);
    }

    futures::future::join_all(tasks).await;

    let deposits = detected_deposits.lock().await.clone();
    let final_blocks = latest_blocks.lock().await.clone();
    Ok(IndexerResult {
        deposits,
        latest_blocks: final_blocks,
    })
}
