use hashbrown::HashSet;
use mockito::Matcher;
use rustplorer::{AssetConfig, solana::SolanaScanner};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

#[tokio::test]
async fn solana_scan_uses_address_signatures_dedupes_and_fetches_transaction() {
    let mut server = mockito::Server::new_async().await;
    let target = "Target111111111111111111111111111111111111";
    let sender = "Sender111111111111111111111111111111111111";
    let sig = "Sig111111111111111111111111111111111111111111111111111111111111111";

    let sigs_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": [
            { "signature": "TooNew1111111111111111111111111111111111111111111111111111111111", "slot": 25 },
            { "signature": sig, "slot": 12 },
            { "signature": "TooOld11111111111111111111111111111111111111111111111111111111111", "slot": 9 }
        ]
    });

    let _sig_mock = server
        .mock("POST", "/")
        .match_body(Matcher::PartialJson(serde_json::json!({
            "method": "getSignaturesForAddress",
            "params": [
                target,
                {
                    "limit": 1000,
                    "minContextSlot": 10,
                    "commitment": "confirmed"
                }
            ]
        })))
        .with_status(200)
        .with_body(sigs_body.to_string())
        .create_async()
        .await;

    let tx_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "slot": 12,
            "transaction": {
                "signatures": [sig],
                "message": { "accountKeys": [sender, target] }
            },
            "meta": {
                "preBalances": [1_000_000_000u64, 0u64],
                "postBalances": [499_995_000u64, 500_000_000u64],
                "preTokenBalances": [],
                "postTokenBalances": []
            }
        }
    });

    let tx_mock = server
        .mock("POST", "/")
        .match_body(Matcher::PartialJson(serde_json::json!({
            "method": "getTransaction",
            "params": [
                sig,
                {
                    "encoding": "json",
                    "maxSupportedTransactionVersion": 0,
                    "commitment": "confirmed"
                }
            ]
        })))
        .with_status(200)
        .with_body(tx_body.to_string())
        .expect(1)
        .create_async()
        .await;

    let scanner = SolanaScanner {
        rpc_urls: vec![server.url()],
        caip2: "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp".to_string(),
        name: "solana".to_string(),
        assets: HashMap::new(),
        rpc_delay_ms: Some(0),
        max_concurrent: 2,
    };

    let mut targets = HashSet::new();
    targets.insert(target.to_string());
    let (tx, mut rx) = mpsc::channel(4);

    scanner
        .scan(reqwest::Client::new(), 10, 20, Arc::new(targets), tx)
        .await
        .expect("scan succeeds");

    let deposit = rx.recv().await.expect("native deposit emitted");
    assert_eq!(deposit.chain, "solana");
    assert_eq!(deposit.asset, "Native");
    assert_eq!(deposit.from_address, sender);
    assert_eq!(deposit.to_address, target);
    assert_eq!(deposit.amount_raw, "500000000");
    assert_eq!(deposit.amount_clean, "0.5");
    assert_eq!(deposit.block_number, 12);
    assert_eq!(deposit.tx_hash, sig);
    assert!(rx.try_recv().is_err(), "only one deposit should be emitted");

    tx_mock.assert_async().await;
}

#[tokio::test]
async fn solana_scan_queries_token_accounts_for_spl_owner_deposits() {
    let mut server = mockito::Server::new_async().await;
    let owner = "Owner1111111111111111111111111111111111111";
    let token_account = "TokenAcct111111111111111111111111111111111";
    let sender = "Sender111111111111111111111111111111111111";
    let mint = "Mint11111111111111111111111111111111111111";
    let sig = "SplSig111111111111111111111111111111111111111111111111111111111111";

    let _token_accounts_mock = server
        .mock("POST", "/")
        .match_body(Matcher::PartialJson(serde_json::json!({
            "method": "getTokenAccountsByOwner",
            "params": [
                owner,
                { "mint": mint },
                { "encoding": "jsonParsed", "commitment": "confirmed" }
            ]
        })))
        .with_status(200)
        .with_body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": { "value": [ { "pubkey": token_account } ] }
            })
            .to_string(),
        )
        .create_async()
        .await;

    let _owner_sigs_mock = server
        .mock("POST", "/")
        .match_body(Matcher::PartialJson(serde_json::json!({
            "method": "getSignaturesForAddress",
            "params": [owner, { "limit": 1000, "minContextSlot": 10, "commitment": "confirmed" }]
        })))
        .with_status(200)
        .with_body(serde_json::json!({ "jsonrpc": "2.0", "id": 1, "result": [] }).to_string())
        .expect(1)
        .create_async()
        .await;

    let _token_sigs_mock = server
        .mock("POST", "/")
        .match_body(Matcher::PartialJson(serde_json::json!({
            "method": "getSignaturesForAddress",
            "params": [token_account, { "limit": 1000, "minContextSlot": 10, "commitment": "confirmed" }]
        })))
        .with_status(200)
        .with_body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": [
                    { "signature": sig, "slot": 12 },
                    { "signature": "SplTooOld11111111111111111111111111111111111111111111111111111", "slot": 9 }
                ]
            })
            .to_string(),
        )
        .expect(1)
        .create_async()
        .await;

    let tx_mock = server
        .mock("POST", "/")
        .match_body(Matcher::PartialJson(serde_json::json!({
            "method": "getTransaction",
            "params": [sig, { "encoding": "json", "maxSupportedTransactionVersion": 0, "commitment": "confirmed" }]
        })))
        .with_status(200)
        .with_body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "slot": 12,
                    "transaction": {
                        "signatures": [sig],
                        "message": { "accountKeys": [sender, token_account] }
                    },
                    "meta": {
                        "preBalances": [1_000_000_000u64, 0u64],
                        "postBalances": [999_995_000u64, 0u64],
                        "preTokenBalances": [],
                        "postTokenBalances": [
                            {
                                "owner": owner,
                                "mint": mint,
                                "uiTokenAmount": { "amount": "99000000", "decimals": 6 }
                            }
                        ]
                    }
                }
            })
            .to_string(),
        )
        .expect(1)
        .create_async()
        .await;

    let scanner = SolanaScanner {
        rpc_urls: vec![server.url()],
        caip2: "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp".to_string(),
        name: "solana".to_string(),
        assets: HashMap::from([(
            "PYUSDT".to_string(),
            AssetConfig {
                contract: mint.to_string(),
                decimals: 6,
            },
        )]),
        rpc_delay_ms: Some(0),
        max_concurrent: 2,
    };

    let mut targets = HashSet::new();
    targets.insert(owner.to_string());
    let (tx, mut rx) = mpsc::channel(4);

    scanner
        .scan(reqwest::Client::new(), 10, 20, Arc::new(targets), tx)
        .await
        .expect("scan succeeds");

    let deposit = rx.recv().await.expect("SPL deposit emitted");
    assert_eq!(deposit.chain, "solana");
    assert_eq!(deposit.asset, "PYUSDT");
    assert_eq!(deposit.to_address, owner);
    assert_eq!(deposit.amount_raw, "99000000");
    assert_eq!(deposit.amount_clean, "99");
    assert_eq!(deposit.block_number, 12);
    assert_eq!(deposit.tx_hash, sig);

    tx_mock.assert_async().await;
}
