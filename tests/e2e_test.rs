use hashbrown::HashSet;
use rustplorer::*;
use std::collections::HashMap;
use std::sync::Arc;

const ANVIL_RPC: &str = "http://127.0.0.1:8545";
const SOLANA_RPC: &str = "http://localhost:8899";

const FROM: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";
const TO: &str = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8";
const TOKEN_ADDR: &str = "0x9fE46736679d2D9a65F0992F2272dE9f3c7fa6e0";

const SOL_RECEIVER: &str = "3zCGKxMK3JHNUMtHbticPoDvoRbUgzY65ayoHMWZwZE2";

async fn anvil_running() -> bool {
    reqwest::Client::new()
        .post(ANVIL_RPC)
        .json(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}))
        .send()
        .await
        .is_ok()
}

async fn solana_running() -> bool {
    reqwest::Client::new()
        .post(SOLANA_RPC)
        .json(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"getHealth"}))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

#[tokio::test]
#[ignore]
async fn e2e_evm_native_eth_deposit() {
    if !anvil_running().await {
        eprintln!("Skipping: anvil not running at {}", ANVIL_RPC);
        return;
    }

    let chains = vec![ChainConfig {
        caip2: "eip155:31337".to_string(),
        rpc: vec![ANVIL_RPC.to_string()],
        start_block: Some(0),
        end_block: Some(10),
    }];

    let mut assets = HashMap::new();
    assets.insert(
        "ETH".to_string(),
        AssetConfig {
            network: "eip155:31337".to_string(),
            contract: "native".to_string(),
            decimals: 18,
        },
    );

    let mut targets = HashSet::new();
    targets.insert(TO.to_lowercase().to_string());

    let results = run_indexer(chains, assets, Arc::new(targets))
        .await
        .unwrap()
        .deposits;

    eprintln!("EVM native deposits found: {}", results.len());
    for d in &results {
        eprintln!(
            "  block {} | {} -> {} | {} ETH (raw: {})",
            d.block_number, d.from_address, d.to_address, d.amount_clean, d.amount_raw
        );
    }

    let native: Vec<_> = results.iter().filter(|r| r.token == "Native").collect();
    assert!(!native.is_empty(), "Should detect native ETH transfer");

    let deposit = native
        .iter()
        .find(|r| r.to_address == TO.to_lowercase())
        .unwrap();
    assert_eq!(deposit.amount_clean, "1");
    assert_eq!(deposit.from_address, FROM.to_lowercase());
    assert_eq!(deposit.to_address, TO.to_lowercase());
}

#[tokio::test]
#[ignore]
async fn e2e_evm_erc20_deposit() {
    if !anvil_running().await {
        eprintln!("Skipping: anvil not running at {}", ANVIL_RPC);
        return;
    }

    let chains = vec![ChainConfig {
        caip2: "eip155:31337".to_string(),
        rpc: vec![ANVIL_RPC.to_string()],
        start_block: Some(0),
        end_block: Some(10),
    }];

    let mut assets = HashMap::new();
    assets.insert(
        "MTK".to_string(),
        AssetConfig {
            network: "eip155:31337".to_string(),
            contract: TOKEN_ADDR.to_lowercase().to_string(),
            decimals: 6,
        },
    );

    let mut targets = HashSet::new();
    targets.insert(TO.to_lowercase().to_string());

    let results = run_indexer(chains, assets, Arc::new(targets))
        .await
        .unwrap()
        .deposits;

    eprintln!("EVM ERC20 deposits found: {}", results.len());
    for d in &results {
        eprintln!(
            "  block {} | {} -> {} | {} MTK (raw: {})",
            d.block_number, d.from_address, d.to_address, d.amount_clean, d.amount_raw
        );
    }

    let erc20: Vec<_> = results.iter().filter(|r| r.token == "MTK").collect();
    assert!(!erc20.is_empty(), "Should detect ERC20 transfer");

    let transfer_deposit = erc20
        .iter()
        .find(|r| r.from_address == FROM.to_lowercase())
        .unwrap();
    assert_eq!(transfer_deposit.amount_clean, "50");
    assert_eq!(transfer_deposit.to_address, TO.to_lowercase());
}

#[tokio::test]
#[ignore]
async fn e2e_solana_native_deposit() {
    if !solana_running().await {
        eprintln!(
            "Skipping: solana-test-validator not running at {}",
            SOLANA_RPC
        );
        return;
    }

    let client = reqwest::Client::new();
    let res: serde_json::Value = client
        .post(SOLANA_RPC)
        .json(&serde_json::json!({"jsonrpc":"2.0","id":1,"method":"getSlot"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let current_slot = res["result"].as_u64().unwrap();
    let start_slot = current_slot.saturating_sub(500);

    let chains = vec![ChainConfig {
        caip2: "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp".to_string(),
        rpc: vec![SOLANA_RPC.to_string()],
        start_block: Some(start_slot),
        end_block: Some(current_slot),
    }];

    let mut assets = HashMap::new();
    assets.insert(
        "SOL".to_string(),
        AssetConfig {
            network: "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp".to_string(),
            contract: "native".to_string(),
            decimals: 9,
        },
    );

    let mut targets = HashSet::new();
    targets.insert(SOL_RECEIVER.to_string());

    let results = run_indexer(chains, assets, Arc::new(targets))
        .await
        .unwrap()
        .deposits;

    eprintln!("Solana native deposits found: {}", results.len());
    for d in &results {
        eprintln!(
            "  slot {} | {} -> {} | {} SOL",
            d.block_number, d.from_address, d.to_address, d.amount_clean
        );
    }

    assert!(
        !results.is_empty(),
        "Should detect the 2.5 SOL transfer to receiver"
    );

    let deposit = &results[0];
    assert_eq!(deposit.token, "Native");
    assert_eq!(deposit.to_address, SOL_RECEIVER);
    assert_eq!(deposit.amount_clean, "2.5");
}

#[tokio::test]
#[ignore]
async fn e2e_evm_auto_end_block() {
    if !anvil_running().await {
        eprintln!("Skipping: anvil not running at {}", ANVIL_RPC);
        return;
    }

    let chains = vec![ChainConfig {
        caip2: "eip155:31337".to_string(),
        rpc: vec![ANVIL_RPC.to_string()],
        start_block: Some(0),
        end_block: None,
    }];

    let mut assets = HashMap::new();
    assets.insert(
        "ETH".to_string(),
        AssetConfig {
            network: "eip155:31337".to_string(),
            contract: "native".to_string(),
            decimals: 18,
        },
    );

    let mut targets = HashSet::new();
    targets.insert(TO.to_lowercase().to_string());

    let results = run_indexer(chains, assets, Arc::new(targets))
        .await
        .unwrap()
        .deposits;

    eprintln!("Auto end_block: found {} deposits", results.len());
    assert!(
        !results.is_empty(),
        "end_block=None should auto-detect tip and find native ETH deposits"
    );
}
