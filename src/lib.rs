pub mod btc;
pub mod evm;
pub mod format;
pub mod rpc;
pub mod solana;

use alloy_primitives::Address;
use hashbrown::HashSet;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

pub use format::format_to_human;

pub const DEFAULT_EVM_LOOKBACK: u64 = 1_000;
pub const DEFAULT_SOLANA_LOOKBACK: u64 = 500;
pub const DEFAULT_BTC_LOOKBACK: u64 = 6;

// ---------------------------------------------------------------------------
// Configuration Types — nested [chains.NAME] with [chains.NAME.assets.X]
// ---------------------------------------------------------------------------

/// Top-level configuration: `[chains.NAME]` maps to a HashMap keyed by chain name.
///
/// Example TOML:
/// ```toml
/// [chains.ethereum]
/// caip2 = "eip155:1"
/// rpc = ["https://ethereum.publicnode.com"]
///
///   [chains.ethereum.assets.ETH_NATIVE]
///   contract = "native"
///   decimals = 18
/// ```
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct AppConfig {
    pub chains: HashMap<String, ChainConfig>,
}

/// Per-chain configuration under `[chains.<name>]`.
///
/// Assets are nested directly inside the chain, eliminating the need for a
/// separate global `[assets]` section with redundant `caip2` fields.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ChainConfig {
    pub caip2: String,
    pub rpc: Vec<String>,
    #[serde(default)]
    pub start_block: Option<u64>,
    #[serde(default)]
    pub end_block: Option<u64>,
    #[serde(default)]
    pub rpc_options: Option<RpcOptions>,
    #[serde(default)]
    pub assets: HashMap<String, AssetConfig>,
}

/// Rate-limiting / concurrency options for a chain, under
/// `[chains.<name>.rpc_options]`.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct RpcOptions {
    #[serde(default)]
    pub max_concurrent: Option<usize>,
    #[serde(default)]
    pub delay_ms: Option<u64>,
}

/// Per-asset configuration under `[chains.<name>.assets.<ticker>]`.
///
/// Unlike the old flat structure, `caip2` is no longer needed here — it is
/// inherited from the parent `ChainConfig`.
#[derive(Debug, Deserialize, Clone, Serialize)]
pub struct AssetConfig {
    pub contract: String,
    pub decimals: u32,
}

// ---------------------------------------------------------------------------
// Deposit Result
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DepositResult {
    pub chain: String,
    pub asset: String,
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

// ---------------------------------------------------------------------------
// Config Loading
// ---------------------------------------------------------------------------

pub fn load_config(path: &std::path::Path) -> Result<AppConfig, anyhow::Error> {
    let config_str = std::fs::read_to_string(path)?;
    let config: AppConfig = toml_edit::de::from_str(&config_str)?;
    Ok(config)
}

// ---------------------------------------------------------------------------
// Address Loading
// ---------------------------------------------------------------------------

/// Load target addresses from a text file.
///
/// EVM addresses (starting with `0x`/`0X`) are validated using `alloy_primitives::Address`
/// to ensure cryptographic correctness, then stored in lowercase for consistent matching.
/// Solana and Bitcoin addresses are stored as-is.
pub fn load_addresses(path: &std::path::Path) -> Result<HashSet<String>, anyhow::Error> {
    let file = std::fs::File::open(path)?;
    let mut set = HashSet::with_capacity(1_000_000);
    use std::io::{BufRead, BufReader};

    for line in BufReader::new(file).lines() {
        let addr = line?.trim().to_string();
        if addr.is_empty() {
            continue;
        }

        if addr.starts_with("0x") || addr.starts_with("0X") {
            match addr.parse::<Address>() {
                Ok(alloy_addr) => {
                    set.insert(alloy_addr.to_string().to_lowercase());
                }
                Err(e) => {
                    tracing::warn!("Skipping invalid EVM address {}: {}", addr, e);
                }
            }
        } else {
            set.insert(addr);
        }
    }
    Ok(set)
}

// ---------------------------------------------------------------------------
// Internal Helpers
// ---------------------------------------------------------------------------

fn default_lookback(caip2: &str) -> u64 {
    if caip2.starts_with("solana:") {
        DEFAULT_SOLANA_LOOKBACK
    } else if caip2.starts_with("bip122:") {
        DEFAULT_BTC_LOOKBACK
    } else {
        DEFAULT_EVM_LOOKBACK
    }
}

/// Extract `rpc_delay_ms` and `max_concurrent` from `ChainConfig.rpc_options`,
/// falling back to sensible defaults per chain type.
fn resolve_rpc_params(chain: &ChainConfig) -> (Option<u64>, usize) {
    let delay_ms = chain.rpc_options.as_ref().and_then(|o| o.delay_ms);
    let max_concurrent = chain
        .rpc_options
        .as_ref()
        .and_then(|o| o.max_concurrent)
        .unwrap_or_else(|| {
            if chain.caip2.starts_with("solana:") {
                1
            } else if chain.caip2.starts_with("bip122:") {
                3
            } else {
                5
            }
        });
    (delay_ms, max_concurrent)
}

// ---------------------------------------------------------------------------
// Core Indexer
// ---------------------------------------------------------------------------

/// Run the core indexer across all configured chains.
///
/// Uses MPSC channels for non-blocking result aggregation instead of
/// shared `Arc<Mutex<Vec<DepositResult>>>`. Each scanner sends deposits
/// through a `tokio::sync::mpsc::Sender`, and a single receiver collects
/// them — adhering to the Rust concurrency mantra:
/// *"Do not communicate by sharing memory; instead, share memory by communicating."*
pub async fn run_indexer(
    chains: HashMap<String, ChainConfig>,
    targets: Arc<HashSet<String>>,
) -> Result<IndexerResult, anyhow::Error> {
    let (tx, mut rx) = mpsc::channel::<DepositResult>(50_000);
    let latest_blocks: Arc<tokio::sync::Mutex<HashMap<String, u64>>> =
        Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let client = reqwest::Client::new();

    let mut tasks: Vec<JoinHandle<()>> = vec![];

    for (chain_name, chain) in chains {
        if chain.rpc.is_empty() {
            continue;
        }

        let tx_clone = tx.clone();
        let targets_clone = Arc::clone(&targets);
        let blocks_clone = Arc::clone(&latest_blocks);
        let client_clone = client.clone();
        let rpc_clone = chain.rpc.clone();
        let caip2 = chain.caip2.clone();
        let lookback = default_lookback(&caip2);
        let (rpc_delay_ms, max_concurrent) = resolve_rpc_params(&chain);
        let chain_assets = chain.assets.clone();

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
                tracing::warn!(
                    chain = %caip2,
                    start = start_block,
                    end = end_block,
                    "start_block > end_block, skipping"
                );
                return;
            }

            tracing::info!(
                caip2 = %caip2,
                name = %chain_name,
                start = start_block,
                end = end_block,
                "Scanning blocks"
            );

            if is_evm {
                let scanner = evm::EvmScanner {
                    rpc_urls: rpc_clone,
                    caip2,
                    name: chain_name,
                    assets: chain_assets,
                    rpc_delay_ms,
                    max_concurrent,
                };
                let _ = scanner
                    .scan(
                        client_clone,
                        start_block,
                        end_block,
                        targets_clone,
                        tx_clone,
                    )
                    .await;
            } else if is_solana {
                let scanner = solana::SolanaScanner {
                    rpc_urls: rpc_clone,
                    caip2,
                    name: chain_name,
                    assets: chain_assets,
                    rpc_delay_ms,
                    max_concurrent,
                };
                let _ = scanner
                    .scan(
                        client_clone,
                        start_block,
                        end_block,
                        targets_clone,
                        tx_clone,
                    )
                    .await;
            } else if is_btc {
                let scanner = btc::BtcScanner {
                    rpc_urls: rpc_clone,
                    caip2,
                    name: chain_name,
                    assets: chain_assets,
                    rpc_delay_ms,
                    max_concurrent,
                };
                let _ = scanner
                    .scan(
                        client_clone,
                        start_block,
                        end_block,
                        targets_clone,
                        tx_clone,
                    )
                    .await;
            } else {
                tracing::warn!(caip2 = %caip2, "Unsupported network");
            }
        });
        tasks.push(task);
    }

    // Drop the original sender so the receiver loop can terminate
    // when all tasks have finished sending.
    drop(tx);

    // Collect deposits from the channel. Watch mode owns SIGINT handling so it
    // can terminate the polling loop instead of starting another cycle.
    let mut deposits = Vec::new();

    while let Some(deposit) = rx.recv().await {
        deposits.push(deposit);
    }

    let _ = futures::future::join_all(tasks).await;

    let final_blocks = latest_blocks.lock().await.clone();
    Ok(IndexerResult {
        deposits,
        latest_blocks: final_blocks,
    })
}
