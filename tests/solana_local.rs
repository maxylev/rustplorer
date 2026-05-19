use hashbrown::HashSet;
use rustplorer::*;
use std::collections::HashMap;
use std::sync::Arc;

async fn solana_available(url: &str) -> bool {
    let client = reqwest::Client::new();
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getHealth"
    });
    matches!(
        client.post(url).json(&payload).send().await,
        Ok(res) if res.status().is_success()
    )
}

#[tokio::test]
#[ignore]
async fn test_solana_local_validator_connectivity() {
    let url = "http://localhost:8899";
    if !solana_available(url).await {
        eprintln!("Skipping: solana-test-validator not running at {}", url);
        return;
    }

    let client = reqwest::Client::new();
    let slot_payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getSlot"
    });

    let res: serde_json::Value = client
        .post(url)
        .json(&slot_payload)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let current_slot = res["result"].as_u64().unwrap();
    let start_slot = current_slot.saturating_sub(5);

    let chains = vec![ChainConfig {
        caip2: "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp".to_string(),
        rpc: vec![url.to_string()],
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

    let targets = Arc::new(HashSet::new());
    let results = run_indexer(chains, assets, targets).await.unwrap();
    assert!(results.is_empty(), "No targets loaded, should find nothing");
}

#[tokio::test]
#[ignore]
async fn test_solana_local_validator_detect_airdrop() {
    let url = "http://localhost:8899";
    if !solana_available(url).await {
        eprintln!("Skipping: solana-test-validator not running at {}", url);
        return;
    }

    let client = reqwest::Client::new();

    let slot_payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getSlot"
    });
    let res: serde_json::Value = client
        .post(url)
        .json(&slot_payload)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let current_slot = res["result"].as_u64().unwrap();
    let start_slot = current_slot.saturating_sub(10);

    let chains = vec![ChainConfig {
        caip2: "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp".to_string(),
        rpc: vec![url.to_string()],
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
    let known_address = "11111111111111111111111111111111";
    targets.insert(known_address.to_string());

    let results = run_indexer(chains, assets, Arc::new(targets))
        .await
        .unwrap();

    eprintln!(
        "Found {} deposits for system program address",
        results.len()
    );
    for deposit in &results {
        eprintln!(
            "  slot {} | {} -> {} | {} SOL",
            deposit.block_number, deposit.from_address, deposit.to_address, deposit.amount_clean,
        );
    }
}
