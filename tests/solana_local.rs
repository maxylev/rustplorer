use hashbrown::HashSet;
use rustplorer::{AssetConfig, ChainConfig, run_indexer};
use std::collections::HashMap;
use std::process::Command;
use std::sync::Arc;

const RPC_URL: &str = "http://127.0.0.1:8899";
const SOL_CAIP2: &str = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp";

fn run(cmd: &str, args: &[&str]) -> String {
    let output = Command::new(cmd)
        .args(args)
        .output()
        .unwrap_or_else(|_| panic!("{} not found", cmd));
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn solana(args: &[&str]) -> String {
    let mut full_args = vec!["--url", RPC_URL];
    full_args.extend_from_slice(args);
    run("solana", &full_args)
}

fn solana_keygen(keyfile: &str) -> String {
    run(
        "solana-keygen",
        &[
            "new",
            "-o",
            keyfile,
            "--no-bip39-passphrase",
            "--force",
            "--silent",
        ],
    )
}

async fn solana_slot() -> u64 {
    let client = reqwest::Client::new();
    let resp: serde_json::Value = client
        .post(RPC_URL)
        .json(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "getSlot", "params": []
        }))
        .send()
        .await
        .expect("RPC call failed")
        .json()
        .await
        .expect("RPC parse failed");
    resp["result"].as_u64().expect("no slot in result")
}

async fn signature_slot_for_address(addr: &str, min_slot: u64) -> Option<u64> {
    let client = reqwest::Client::new();
    let resp: serde_json::Value = client
        .post(RPC_URL)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getSignaturesForAddress",
            "params": [
                addr,
                {
                    "limit": 10,
                    "minContextSlot": min_slot,
                    "commitment": "confirmed"
                }
            ]
        }))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;

    resp["result"].as_array()?.iter().find_map(|entry| {
        let slot = entry["slot"].as_u64()?;
        (slot >= min_slot).then_some(slot)
    })
}

#[tokio::test]
async fn detect_sol_native_deposit() {
    // Skip if solana-test-validator is not running on localhost:8899.
    // This test requires the validator started by tests/scripts/test_e2e_full.sh
    // (or tests/scripts/demo.sh) with --slots-per-epoch for proper slot advancement.
    let client = reqwest::Client::new();
    if client
        .post(RPC_URL)
        .json(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "getHealth", "params": []
        }))
        .send()
        .await
        .is_err()
    {
        eprintln!("SKIP: solana-test-validator not running on {}", RPC_URL);
        return;
    }
    let payer_key = "/tmp/solana-test-payer.json";
    let target_key = "/tmp/sol_local_target.json";
    let target_pk_file = "/tmp/sol_local_target.pub";

    let _ = std::fs::remove_file(target_key);
    let _ = std::fs::remove_file(target_pk_file);

    let _ = solana_keygen(target_key);
    let target_addr = solana(&["address", "-k", target_key]).trim().to_string();
    assert!(!target_addr.is_empty(), "failed to generate target keypair");

    let _payer_addr = solana(&["address", "-k", payer_key]).trim().to_string();

    let slot_before = solana_slot().await;

    let transfer = solana(&[
        "transfer",
        "-k",
        payer_key,
        "--allow-unfunded-recipient",
        &target_addr,
        "0.5",
    ]);
    assert!(
        transfer.contains("Signature"),
        "solana transfer did not report a signature: {transfer}"
    );

    let mut slot_after = slot_before;
    let mut signature_slot = None;
    for _ in 0..30 {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        slot_after = solana_slot().await;
        signature_slot = signature_slot_for_address(&target_addr, slot_before).await;
        if signature_slot.is_some() && slot_after > slot_before + 2 {
            break;
        }
    }
    let sig_slot =
        signature_slot.expect("transfer signature not visible via getSignaturesForAddress");
    slot_after = slot_after.max(sig_slot);

    let mut targets = HashSet::new();
    targets.insert(target_addr.clone());

    let mut chains = HashMap::new();
    chains.insert(
        "solana".to_string(),
        ChainConfig {
            caip2: SOL_CAIP2.to_string(),
            rpc: vec![RPC_URL.to_string()],
            start_block: Some(slot_before),
            end_block: Some(slot_after),
            rpc_options: None,
            assets: {
                let mut a = HashMap::new();
                a.insert(
                    "SOL".to_string(),
                    AssetConfig {
                        contract: "native".to_string(),
                        decimals: 9,
                    },
                );
                a
            },
        },
    );

    let result = run_indexer(chains, Arc::new(targets))
        .await
        .expect("indexer failed");

    let native_deposits: Vec<_> = result
        .deposits
        .iter()
        .filter(|d| d.asset == "SOL" && d.to_address == target_addr)
        .collect();

    assert!(
        !native_deposits.is_empty(),
        "signature-based Solana scanner should detect local test-validator transfers"
    );
    let deposit = &native_deposits[0];
    assert_eq!(deposit.amount_clean, "0.5");
    assert_eq!(deposit.amount_raw, "500000000");
    assert_eq!(deposit.chain, "solana");

    let _ = std::fs::remove_file(target_key);
    let _ = std::fs::remove_file(target_pk_file);
}
