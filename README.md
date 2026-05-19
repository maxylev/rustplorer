<h1 align="center">rustplorer</h1>

<p align="center">
  <strong>High-performance multi-chain deposit detector for EVM and Solana blockchains.</strong><br>
  Monitors up to 1 million addresses using <strong>only public RPC endpoints</strong> — no API keys, no third-party services.
</p>

<p align="center">
  <a href="https://crates.io/crates/rustplorer"><img src="https://img.shields.io/crates/v/rustplorer.svg" alt="crates.io"></a>
  <img src="https://img.shields.io/crates/l/rustplorer.svg" alt="License">
  <img src="https://img.shields.io/github/actions/workflow/status/maxylev/rustplorer/ci.yml?branch=main" alt="CI">
</p>

---

## Features

- **Multi-chain**: Ethereum, Base, Polygon, BSC, Arbitrum, and any EVM chain + Solana
- **Multi-token**: Native tokens (ETH, MATIC, SOL) and ERC-20 / SPL tokens
- **No API keys**: Works with any public JSON-RPC endpoint
- **Multi-RPC failover**: Automatically retries on the next endpoint if one fails or rate-limits
- **1M+ addresses**: Loads addresses into an in-memory `HashSet` for O(1) matching
- **Human-readable output**: Converts raw hex/lamport values to decimal strings
- **Dual use**: CLI binary and Rust library crate
- **Extensible**: Modular architecture — add Bitcoin, Monero, or any chain by implementing a scanner
- **Optional block range**: Omit `start_block`/`end_block` to auto-detect from the node

## Architecture

```
Blockchain RPC ──► Block Stream ──► Local HashSet Lookup ──► Deposit Match
                                        (1M addresses)
```

Instead of querying "does address X have a deposit?" (pull), rustplorer downloads blocks and asks "does this block contain any of my addresses?" (push). All filtering happens locally.

## Installation

### From crates.io

```bash
cargo install rustplorer
```

### From source

```bash
git clone https://github.com/maxylev/rustplorer.git
cd rustplorer
cargo install --path .
```

### As a library

```toml
[dependencies]
rustplorer = "0.1"
```

## Quick Start

### 1. Create a config file (`Config.toml`)

Both `start_block` and `end_block` are optional. If omitted, the latest block is fetched from the node.

```toml
[[chains]]
caip2 = "eip155:1"
rpc = [
    "https://eth.llamarpc.com",
    "https://rpc.ankr.com/eth",
    "https://cloudflare-eth.com",
]
start_block = 19000000
end_block = 19000500

[[chains]]
caip2 = "eip155:8453"
rpc = ["https://mainnet.base.org"]
start_block = 12000000
end_block = 12000500

# Omit start_block and end_block to scan the latest block only
[[chains]]
caip2 = "eip155:137"
rpc = ["https://polygon-rpc.com"]

[[chains]]
caip2 = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp"
rpc = ["https://api.mainnet-beta.solana.com"]
start_block = 250000000
end_block = 250000100

[assets.ETH_NATIVE]
network = "eip155:1"
contract = "native"
decimals = 18

[assets.USDC_ETH]
network = "eip155:1"
contract = "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"
decimals = 6

[assets.USDC_BASE]
network = "eip155:8453"
contract = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"
decimals = 6

[assets.SOL_NATIVE]
network = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp"
contract = "native"
decimals = 9

[assets.USDC_SOL]
network = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp"
contract = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
decimals = 6
```

### 2. Create an addresses file (`addresses.txt`)

One address per line — supports mixed EVM and Solana:

```
0x71C7656EC7ab88b098defB751B7401B5f6d8976F
0x8Ba1f109551bD432803012645Ac136ddd64DBA72
AMYmXa54xZuS7rjeSX7E4YwNVKpNbhFHK9gP7jLCN3A
```

### 3. Run

```bash
# Output JSON to stdout
rustplorer --addresses addresses.txt

# Save as JSON file
rustplorer -a addresses.txt -o deposits.json

# Save as CSV
rustplorer -a addresses.txt --format csv -o deposits.csv

# Override block range for a specific network
rustplorer -a addresses.txt --network eip155:137 --start-block 55000000 --end-block 55001000

# Override RPC endpoints
rustplorer -a addresses.txt --network eip155:1 --rpc "https://rpc.ankr.com/eth,https://eth.llamarpc.com"

# Verbose mode
rustplorer -a addresses.txt --verbose -o results.json
```

## CLI Reference

```
rustplorer [OPTIONS] --addresses <FILE>

Options:
  -a, --addresses <FILE>     Text file with target addresses (one per line)
  -c, --config <FILE>        Config file [default: Config.toml]
  -f, --format <FORMAT>      Output format: json, csv [default: json]
  -o, --output <FILE>        Save to file (stdout if omitted)
      --network <CAIP2>      Filter to a single network (e.g. eip155:1)
      --start-block <N>      Override start block (node default if omitted)
      --end-block <N>        Override end block (node default if omitted)
      --rpc <URLS>           Override RPC endpoints (comma-separated)
      --verbose              Show progress output
  -h, --help                 Show help
  -V, --version              Show version
```

## Output Format

### JSON

```json
[
  {
    "chain": "eip155:8453",
    "token": "USDC_BASE",
    "from_address": "0x20f3a60a7ff2411e7ca1bf8ef9a0994336021f1a",
    "to_address": "0x71c7656ec7ab88b098defb751b7401b5f6d8976f",
    "amount_raw": "0x0000000000000000000000000000000000000000000000000000000002faf080",
    "amount_clean": "50",
    "block_number": 12000542
  },
  {
    "chain": "eip155:1",
    "token": "Native",
    "from_address": "0xd8da6bf26964af9d7eed9e03e53415d37aa96045",
    "to_address": "0x01bf3a00a11a417eef11a8aa0aa341bd7aa010fa",
    "amount_raw": "0xde0b6b3a7640000",
    "amount_clean": "1",
    "block_number": 19000210
  },
  {
    "chain": "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp",
    "token": "Native",
    "from_address": "E45GKD1qqErzCaRKnLbZppPpDPeiLJVz3e44dNmELiqC",
    "to_address": "3zCGKxMK3JHNUMtHbticPoDvoRbUgzY65ayoHMWZwZE2",
    "amount_raw": "2500000000",
    "amount_clean": "2.5",
    "block_number": 263
  }
]
```

### CSV

```csv
chain,token,from_address,to_address,amount_raw,amount_clean,block_number
eip155:8453,USDC_BASE,0x20f3a60a...,0x71c7656e...,0x...02faf080,50,12000542
eip155:1,Native,0xd8da6bf2...,0x01bf3a00...,0x0de0b6b3a7640000,1,19000210
solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp,Native,E45GKD1q...,3zCGKxMK...,2500000000,2.5,263
```

## Programmatic Usage

```rust
use rustplorer::{run_indexer, ChainConfig, AssetConfig, DepositResult};
use hashbrown::HashSet;
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    let mut targets = HashSet::new();
    targets.insert("0x71c7656ec7ab88b098defb751b7401b5f6d8976f".to_string());

    let chains = vec![ChainConfig {
        caip2: "eip155:1".to_string(),
        rpc: vec!["https://eth.llamarpc.com".to_string()],
        start_block: Some(19000000),
        end_block: Some(19000100),
    }];

    let mut assets = HashMap::new();
    assets.insert("ETH".to_string(), AssetConfig {
        network: "eip155:1".to_string(),
        contract: "native".to_string(),
        decimals: 18,
    });

    let deposits: Vec<DepositResult> = run_indexer(
        chains,
        assets,
        Arc::new(targets),
    )
    .await
    .unwrap();

    for d in &deposits {
        println!("{} {} {} -> {} ({})",
            d.token, d.amount_clean, d.from_address, d.to_address, d.chain);
    }
}
```

## How It Works

### EVM Chains

| Token Type | Method | Strategy |
|---|---|---|
| ERC-20 / ERC-721 | `eth_getLogs` | Filters Transfer events by contract address, matches `topic[2]` (to) against local address set |
| Native (ETH, MATIC) | `eth_getBlockByNumber` | Downloads full block with transactions, checks `tx.to` and `tx.value > 0` |

Block ranges are chunked into 200-block intervals to respect public RPC limits.

### Solana

| Token Type | Method | Strategy |
|---|---|---|
| Native SOL | `getBlock` | Compares `preBalances` vs `postBalances` per account |
| SPL Tokens | `getBlock` | Checks `postTokenBalances` for matching owners and mints |

Each slot is fetched individually via `getBlock`.

### Multi-RPC Failover

If an RPC endpoint returns a 429, 5xx, or a JSON-RPC error, rustplorer automatically tries the next endpoint in your `rpc` array. All endpoints are exhausted before failing.

### Block Range Auto-Detection

When `start_block` or `end_block` is omitted in the config:

- **EVM**: Calls `eth_blockNumber` to get the latest block
- **Solana**: Calls `getSlot` to get the latest slot

If both are omitted, only the current tip block/slot is scanned. If only `end_block` is omitted, it scans from `start_block` to the current tip.

## Testing

### Unit tests (mock RPC servers)

```bash
cargo test
```

### E2E tests (requires anvil + solana-test-validator)

```bash
# Start local chains
anvil --host 127.0.0.1 --port 8545 --silent &
solana-test-validator --reset --quiet --rpc-port 8899 &

# Run E2E tests
cargo test --test e2e_test -- --ignored --nocapture
```

The E2E tests perform real transfers on local chains:

| Test | Chain | Token | Verification |
|---|---|---|---|
| `e2e_evm_native_eth_deposit` | Anvil (31337) | Native ETH | 1 ETH transfer detected |
| `e2e_evm_erc20_deposit` | Anvil (31337) | ERC-20 MockToken | 50 MTK transfer detected |
| `e2e_evm_auto_end_block` | Anvil (31337) | Native ETH | Auto end_block resolution |
| `e2e_solana_native_deposit` | Solana (testnet) | Native SOL | 2.5 SOL transfer detected |

## Configuration Reference

### Chain (`[[chains]]`)

| Field | Type | Required | Description |
|---|---|---|---|
| `caip2` | string | yes | CAIP-2 chain ID (e.g. `eip155:1`, `solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp`) |
| `rpc` | string[] | yes | One or more public RPC URLs |
| `start_block` | uint64 | no | First block/slot (defaults to node tip) |
| `end_block` | uint64 | no | Last block/slot (defaults to node tip) |

### Asset (`[assets.NAME]`)

| Field | Type | Required | Description |
|---|---|---|---|
| `network` | string | yes | Must match a chain's `caip2` |
| `contract` | string | yes | Token contract address, or `"native"` for the gas token |
| `decimals` | uint32 | yes | Token decimal places (ETH=18, SOL=9, USDC=6) |

## Performance Notes

Public RPC nodes typically allow 5-10 requests/second. With 200-block chunks:

| Chain | Blocks | RPC Calls (ERC20) | RPC Calls (Native) | Est. Time |
|---|---|---|---|---|
| Ethereum | 500 | ~3 | 500 | ~2 min |
| Base | 500 | ~3 | 500 | ~2 min |
| Solana | 100 slots | 100 | — | ~30 sec |

For production workloads at scale, consider dedicated/archive RPC nodes, self-hosted Reth/Erigon nodes, or indexing services.

## License

MIT OR Apache-2.0
