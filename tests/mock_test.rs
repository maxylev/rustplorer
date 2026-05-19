use hashbrown::HashSet;
use rustplorer::*;
use std::collections::HashMap;
use std::sync::Arc;

fn make_evm_config(rpc_url: &str) -> (Vec<ChainConfig>, HashMap<String, AssetConfig>) {
    let chains = vec![ChainConfig {
        caip2: "eip155:1".to_string(),
        rpc: vec![rpc_url.to_string()],
        start_block: Some(0x121212),
        end_block: Some(0x121212),
    }];

    let mut assets = HashMap::new();
    assets.insert(
        "USDC".to_string(),
        AssetConfig {
            network: "eip155:1".to_string(),
            contract: "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48".to_string(),
            decimals: 6,
        },
    );
    assets.insert(
        "ETH".to_string(),
        AssetConfig {
            network: "eip155:1".to_string(),
            contract: "native".to_string(),
            decimals: 18,
        },
    );

    (chains, assets)
}

fn make_target_address() -> HashSet<String> {
    let mut set = HashSet::new();
    set.insert("0x71c7656ec7ab88b098defb751b7401b5f6d8976f".to_string());
    set
}

#[tokio::test]
async fn test_evm_erc20_deposit_detection() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/")
        .match_body(mockito::Matcher::JsonString(
            r#"{"jsonrpc":"2.0","id":1,"method":"eth_getLogs","params":[{"address":"0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48","fromBlock":"0x121212","toBlock":"0x121212","topics":["0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"]}]}"#
                .to_string(),
        ))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"jsonrpc":"2.0","id":1,"result":[{"address":"0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48","topics":["0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef","0x00000000000000000000000020f3a60a7ff2411e7ca1bf8ef9a0994336021f1a","0x00000000000000000000000071c7656ec7ab88b098defb751b7401b5f6d8976f"],"data":"0x0000000000000000000000000000000000000000000000000000000002faf080","blockNumber":"0x121212","transactionHash":"0xabc123"}]}"#)
        .create_async()
        .await;

    let url = server.url();
    let (chains, assets) = make_evm_config(&url);
    let targets = Arc::new(make_target_address());

    let results = run_indexer(chains, assets, targets).await.unwrap().deposits;

    mock.assert_async().await;
    assert_eq!(results.len(), 1);
    let deposit = &results[0];
    assert_eq!(deposit.chain, "eip155:1");
    assert_eq!(deposit.token, "USDC");
    assert_eq!(
        deposit.to_address,
        "0x71c7656ec7ab88b098defb751b7401b5f6d8976f"
    );
    assert_eq!(
        deposit.from_address,
        "0x20f3a60a7ff2411e7ca1bf8ef9a0994336021f1a"
    );
    assert_eq!(deposit.amount_clean, "50");
    assert_eq!(deposit.block_number, 0x121212);
}

#[tokio::test]
async fn test_evm_native_deposit_detection() {
    let mut server = mockito::Server::new_async().await;

    let mock_logs = server
        .mock("POST", "/")
        .match_body(mockito::Matcher::Regex("eth_getLogs".to_string()))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"jsonrpc":"2.0","id":1,"result":[]}"#)
        .create_async()
        .await;

    let mock_block = server
        .mock("POST", "/")
        .match_body(mockito::Matcher::Regex("eth_getBlockByNumber".to_string()))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"jsonrpc":"2.0","id":1,"result":{"number":"0x121212","transactions":[{"from":"0xd8da6bf26964af9d7eed9e03e53415d37aa96045","to":"0x71c7656ec7ab88b098defb751b7401b5f6d8976f","value":"0xde0b6b3a7640000","hash":"0xdef456"}]}}"#)
        .create_async()
        .await;

    let url = server.url();
    let (chains, assets) = make_evm_config(&url);
    let targets = Arc::new(make_target_address());

    let results = run_indexer(chains, assets, targets).await.unwrap().deposits;

    mock_logs.assert_async().await;
    mock_block.assert_async().await;

    let native_deposits: Vec<_> = results.iter().filter(|r| r.token == "Native").collect();
    assert_eq!(native_deposits.len(), 1);
    let deposit = native_deposits[0];
    assert_eq!(deposit.amount_clean, "1");
    assert_eq!(
        deposit.from_address,
        "0xd8da6bf26964af9d7eed9e03e53415d37aa96045"
    );
}

#[tokio::test]
async fn test_evm_no_match() {
    let mut server = mockito::Server::new_async().await;

    server
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"jsonrpc":"2.0","id":1,"result":[]}"#)
        .create_async()
        .await;

    let url = server.url();
    let (chains, assets) = make_evm_config(&url);
    let targets = Arc::new(make_target_address());

    let results = run_indexer(chains, assets, targets).await.unwrap().deposits;
    assert!(results.is_empty());
}

#[tokio::test]
async fn test_rpc_fallback_on_error() {
    let mut server1 = mockito::Server::new_async().await;
    let mut server2 = mockito::Server::new_async().await;

    let bad_mock = server1
        .mock("POST", "/")
        .with_status(429)
        .with_body("rate limited")
        .expect(1)
        .create_async()
        .await;

    let good_mock = server2
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"jsonrpc":"2.0","id":1,"result":[]}"#)
        .expect_at_least(1)
        .create_async()
        .await;

    let chains = vec![ChainConfig {
        caip2: "eip155:1".to_string(),
        rpc: vec![server1.url(), server2.url()],
        start_block: Some(100),
        end_block: Some(100),
    }];

    let mut assets = HashMap::new();
    assets.insert(
        "ETH".to_string(),
        AssetConfig {
            network: "eip155:1".to_string(),
            contract: "native".to_string(),
            decimals: 18,
        },
    );

    let targets = Arc::new(make_target_address());
    let results = run_indexer(chains, assets, targets).await.unwrap().deposits;

    bad_mock.assert_async().await;
    good_mock.assert_async().await;
    assert!(results.is_empty());
}

#[tokio::test]
async fn test_rpc_fallback_on_json_error() {
    let mut server1 = mockito::Server::new_async().await;
    let mut server2 = mockito::Server::new_async().await;

    server1
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32005,"message":"query timeout"}}"#)
        .expect_at_least(1)
        .create_async()
        .await;

    server2
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"jsonrpc":"2.0","id":1,"result":[]}"#)
        .expect_at_least(1)
        .create_async()
        .await;

    let chains = vec![ChainConfig {
        caip2: "eip155:1".to_string(),
        rpc: vec![server1.url(), server2.url()],
        start_block: Some(100),
        end_block: Some(100),
    }];

    let mut assets = HashMap::new();
    assets.insert(
        "ETH".to_string(),
        AssetConfig {
            network: "eip155:1".to_string(),
            contract: "native".to_string(),
            decimals: 18,
        },
    );

    let targets = Arc::new(make_target_address());
    let results = run_indexer(chains, assets, targets).await.unwrap().deposits;

    assert!(results.is_empty());
}

#[tokio::test]
async fn test_solana_native_deposit_detection() {
    let mut server = mockito::Server::new_async().await;

    let target_addr = "11111111111111111111111111111";
    let mock = server
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "blockhash": "abc",
                    "blockTime": 1234567890,
                    "blockHeight": 100,
                    "transactions": [{
                        "transaction": {
                            "message": {
                                "accountKeys": [
                                    "SenderWallet1111111111111111111111111",
                                    target_addr
                                ]
                            }
                        },
                        "meta": {
                            "err": null,
                            "preBalances": [5000000000_u64, 1000000000_u64],
                            "postBalances": [3000000000_u64, 3000000000_u64],
                            "preTokenBalances": [],
                            "postTokenBalances": []
                        }
                    }]
                }
            })
            .to_string(),
        )
        .create_async()
        .await;

    let chains = vec![ChainConfig {
        caip2: "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp".to_string(),
        rpc: vec![server.url()],
        start_block: Some(100),
        end_block: Some(100),
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
    targets.insert(target_addr.to_string());

    let results = run_indexer(chains, assets, Arc::new(targets))
        .await
        .unwrap()
        .deposits;

    mock.assert_async().await;
    assert_eq!(results.len(), 1);
    let deposit = &results[0];
    assert_eq!(deposit.chain, "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp");
    assert_eq!(deposit.token, "Native");
    assert_eq!(deposit.to_address, target_addr);
    assert_eq!(
        deposit.from_address,
        "SenderWallet1111111111111111111111111"
    );
    assert_eq!(deposit.amount_raw, "2000000000");
    assert_eq!(deposit.amount_clean, "2");
    assert_eq!(deposit.block_number, 100);
}

#[tokio::test]
async fn test_config_loading() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("Config.toml");
    std::fs::write(
        &config_path,
        r#"
[[chains]]
caip2 = "eip155:1"
rpc = ["https://eth.llamarpc.com"]
start_block = 19000000
end_block = 19000500

[assets.USDC]
network = "eip155:1"
contract = "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"
decimals = 6
"#,
    )
    .unwrap();

    let config = load_config(&config_path).unwrap();
    assert_eq!(config.chains.len(), 1);
    assert_eq!(config.chains[0].caip2, "eip155:1");
    assert_eq!(config.chains[0].start_block, Some(19000000));
    assert_eq!(config.chains[0].end_block, Some(19000500));
    assert_eq!(config.assets.len(), 1);
    assert_eq!(config.assets["USDC"].decimals, 6);
}

#[tokio::test]
async fn test_address_loading() {
    let dir = tempfile::tempdir().unwrap();
    let addr_path = dir.path().join("addresses.txt");
    std::fs::write(
        &addr_path,
        "0x71C7656EC7ab88b098defB751B7401B5f6d8976F\n\
         0x8Ba1f109551bD432803012645Ac136ddd64DBA72\n\
         \n\
         SolanaAddress123\n",
    )
    .unwrap();

    let addrs = load_addresses(&addr_path).unwrap();
    assert_eq!(addrs.len(), 3);
    assert!(addrs.contains("0x71c7656ec7ab88b098defb751b7401b5f6d8976f"));
    assert!(addrs.contains("0x8ba1f109551bd432803012645ac136ddd64dba72"));
    assert!(addrs.contains("SolanaAddress123"));
}

#[test]
fn test_format_human_readable() {
    assert_eq!(format_to_human("0xde0b6b3a7640000", 18), "1");
    assert_eq!(format_to_human("0x22b1c8c1227a0000", 18), "2.5");
    assert_eq!(format_to_human("0x5f5e100", 6), "100");
    assert_eq!(format_to_human("2500000000", 9), "2.5");
    assert_eq!(format_to_human("0x0", 18), "0");
    assert_eq!(format_to_human("0x2faf080", 6), "50");
    assert_eq!(format_to_human("0x1", 18), "0.000000000000000001");
    assert_eq!(format_to_human("42", 0), "42");
    assert_eq!(format_to_human("1000000", 6), "1");
}

fn make_btc_config(rpc_url: &str) -> (Vec<ChainConfig>, HashMap<String, AssetConfig>) {
    let chains = vec![ChainConfig {
        caip2: "bip122:000000000019d6689c085ae165831e93".to_string(),
        rpc: vec![rpc_url.to_string()],
        start_block: Some(830000),
        end_block: Some(830000),
    }];

    let mut assets = HashMap::new();
    assets.insert(
        "BTC".to_string(),
        AssetConfig {
            network: "bip122:000000000019d6689c085ae165831e93".to_string(),
            contract: "native".to_string(),
            decimals: 8,
        },
    );

    (chains, assets)
}

#[tokio::test]
async fn test_btc_native_deposit_detection() {
    let mut server = mockito::Server::new_async().await;

    let mock_hash = server
        .mock("POST", "/")
        .match_body(mockito::Matcher::Regex("getblockhash".to_string()))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"jsonrpc":"1.0","id":"rustplorer","result":"00000000000000000001abc"}"#)
        .create_async()
        .await;

    let target_addr = "bc1qtargetaddress1234567890";
    let sender_addr = "bc1qsenderaddress0987654321";
    let mock_block = server
        .mock("POST", "/")
        .match_body(mockito::Matcher::Regex("getblock".to_string()))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "jsonrpc": "1.0",
                "id": "rustplorer",
                "result": {
                    "tx": [
                        {
                            "vin": [{
                                "prevout": {
                                    "scriptPubKey": {
                                        "address": sender_addr
                                    }
                                }
                            }],
                            "vout": [{
                                "value": 1.50000000,
                                "scriptPubKey": {
                                    "address": target_addr
                                }
                            }]
                        }
                    ]
                }
            })
            .to_string(),
        )
        .create_async()
        .await;

    let url = server.url();
    let (chains, assets) = make_btc_config(&url);

    let mut targets = HashSet::new();
    targets.insert(target_addr.to_string());

    let results = run_indexer(chains, assets, Arc::new(targets))
        .await
        .unwrap()
        .deposits;

    mock_hash.assert_async().await;
    mock_block.assert_async().await;

    assert_eq!(results.len(), 1);
    let deposit = &results[0];

    assert_eq!(
        deposit.chain,
        "bip122:000000000019d6689c085ae165831e93"
    );
    assert_eq!(deposit.token, "Native");
    assert_eq!(deposit.to_address, target_addr);
    assert_eq!(deposit.from_address, sender_addr);

    assert_eq!(deposit.amount_raw, "150000000");

    assert_eq!(deposit.amount_clean, "1.5");
    assert_eq!(deposit.block_number, 830000);
}
