<img src="https://raw.githubusercontent.com/maxylev/rustplorer/refs/heads/main/banner.png" alt="High-performance multi-chain deposit detector daemon" />

<p align="center">
  <img src="https://img.shields.io/badge/rust-1.95.0-orange?logo=rust" alt="Rust 1.95.0" />
  <img src="https://img.shields.io/badge/edition-2024-blue" alt="Edition 2024" />
  <img src="https://img.shields.io/badge/chains-EVM%20%7C%20Solana%20%7C%20Bitcoin-green" alt="Multi-chain" />
  <img src="https://img.shields.io/badge/license-MIT%20%7C%20Apache--2.0-purple" alt="License" />
</p>

# rustplorer

**High-performance multi-chain deposit detector daemon** — monitors Ethereum, Solana, Bitcoin, and all EVM-compatible chains using only public RPC endpoints. Zero API keys, zero paid services, zero dependencies on third-party indexing platforms.

Rustplorer continuously watches blockchain blocks for incoming deposits to your tracked addresses and exposes results via a real-time HTTP API with a built-in dashboard UI.

---

## Features

- **Multi-chain support** — EVM (Ethereum, Base, Polygon, Arbitrum, Optimism, BSC, and any `eip155:` chain), Solana, and Bitcoin
- **Batched `eth_getLogs`** — one RPC call per block chunk for all ERC-20 tokens (not N+1 calls)
- **MPSC channel aggregation** — non-blocking deposit collection via `tokio::sync::mpsc` (no `Arc<Mutex<Vec>>`)
- **In-memory ring buffer** — O(1) `/deposits` API reads from a `VecDeque` (cap 100), no disk I/O on every poll
- **Nested config** — `[chains.<name>.assets.<TICKER>]` structure with `caip2` on the chain, not on each asset
- **`toml_edit` mutations** — CLI and API add/remove chains and assets while preserving comments in `Config.toml`
- **EIP-55 address validation** — `alloy-primitives` validates and normalizes EVM addresses
- **Lossless BTC precision** — `serde_json` `arbitrary_precision` + `rust_decimal` for exact satoshi math
- **Structured logging** — `tracing` + `tracing-subscriber` with `RUST_LOG` env-filter
- **Graceful shutdown** — `tokio::select!` with `ctrl_c()` signal handling
- **Hot-reload config** — re-reads `Config.toml` each watch cycle; API/CLI changes take effect on next poll
- **Localhost-only API** — binds to `127.0.0.1` by default for security
- **Built-in dashboard** — dark/light theme, address management, chain/asset settings, custom confirm modal
- **Docker-ready** — single `Dockerfile` with fat LTO release binary

---

## Quick Start

```bash
# Install
cargo install rustplorer

# Create a config file (see Configuration below)
cp Config.example.toml Config.toml

# Create an addresses file
echo "0x70997970c51812dc3a010c7d01b50e0d17dc79c8" > addresses.txt

# Single scan
rustplorer --config Config.toml --addresses addresses.txt

# Daemon mode with API + dashboard on port 3000
rustplorer --config Config.toml --addresses addresses.txt \
  --watch --api-port 3000 --interval 60 --verbose
```

Open `http://localhost:3000/` in your browser to see the dashboard.

---

## Configuration

Rustplorer uses a **nested TOML** structure where chains are named tables and assets are scoped under their parent chain. This eliminates the need for redundant `caip2` fields on every asset.

### Config.toml

```toml
# ==========================================
# ETHEREUM
# ==========================================
[chains.ethereum]
caip2 = "eip155:1"
start_block = 19000000
end_block = 19000500
rpc = [
    "https://ethereum.publicnode.com",
    "https://eth.drpc.org",
    "https://1rpc.io/eth",
]

  [chains.ethereum.assets.ETH_NATIVE]
  contract = "native"
  decimals = 18

  [chains.ethereum.assets.USDC]
  contract = "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"
  decimals = 6


# ==========================================
# BASE
# ==========================================
[chains.base]
caip2 = "eip155:8453"
start_block = 12000000
end_block = 12000500
rpc = [
    "https://base.publicnode.com",
    "https://base.drpc.org",
    "https://mainnet.base.org",
]

  [chains.base.assets.USDC]
  contract = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"
  decimals = 6


# ==========================================
# POLYGON (Optional/Commented out)
# ==========================================
# Polygon public RPCs often require API keys. Commented out by default.
# [chains.polygon]
# caip2 = "eip155:137"
# rpc = [
#     "https://polygon-rpc.com",
#     "https://rpc.ankr.com/polygon",
# ]


# ==========================================
# SOLANA
# ==========================================
[chains.solana]
caip2 = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp"
rpc = [
    "https://api.mainnet-beta.solana.com",
]

  # Grouped rate-limiting / performance options
  [chains.solana.rpc_options]
  max_concurrent = 1
  delay_ms = 500

  [chains.solana.assets.SOL_NATIVE]
  contract = "native"
  decimals = 9

  [chains.solana.assets.USDC]
  contract = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
  decimals = 6


# ==========================================
# BITCOIN
# ==========================================
[chains.bitcoin]
caip2 = "bip122:000000000019d6689c085ae165831e93"
rpc = [
    "https://bitcoin-rpc.publicnode.com",
]

  [chains.bitcoin.assets.BTC_NATIVE]
  contract = "native"
  decimals = 8
```

### Config Fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `chains.<name>.caip2` | string | yes | CAIP-2 chain identifier (e.g. `eip155:1`, `solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp`, `bip122:000000000019d6689c085ae165831e93`) |
| `chains.<name>.rpc` | array | yes | List of RPC endpoint URLs (fallback order) |
| `chains.<name>.start_block` | u64 | no | Override start block (default: `tip - lookback`) |
| `chains.<name>.end_block` | u64 | no | Override end block (default: current tip). EVM-only. |
| `chains.<name>.rpc_options` | table | no | Rate-limiting options (see below) |
| `chains.<name>.assets.<TICKER>.contract` | string | yes | `"native"` for the chain's native token, or the ERC-20/SPL contract address |
| `chains.<name>.assets.<TICKER>.decimals` | u32 | yes | Token decimals (e.g. 18 for ETH, 6 for USDC, 9 for SOL, 8 for BTC) |

### RPC Options

The `[chains.<name>.rpc_options]` sub-table controls concurrency and rate-limiting for a chain:

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `max_concurrent` | usize | 1 (Solana), 3 (BTC), 5 (EVM) | Maximum concurrent RPC requests |
| `delay_ms` | u64 | 200 (Solana), 100 (BTC/EVM) | Delay between request batches in milliseconds |

Solana benefits from `max_concurrent = 1` and `delay_ms = 500` to avoid 429 errors from public endpoints.

### CAIP-2 Identifiers

| Chain | CAIP-2 |
|-------|--------|
| Ethereum Mainnet | `eip155:1` |
| Base | `eip155:8453` |
| Polygon | `eip155:137` |
| Arbitrum One | `eip155:42161` |
| Optimism | `eip155:10` |
| BSC | `eip155:56` |
| Solana Mainnet | `solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp` |
| Bitcoin Mainnet | `bip122:000000000019d6689c085ae165831e93` |
| Anvil (local) | `eip155:31337` |

---

## CLI Usage

```
rustplorer [OPTIONS]

Options:
  -c, --config <PATH>           Config file path [default: Config.toml]
  -a, --addresses <PATH>        Text file with target addresses (one per line)
  -f, --format <FORMAT>         Output format: json, csv [default: json]
  -o, --output <PATH>           Save output to file (stdout if omitted)
      --network <CAIP2>         Override network to scan (e.g. eip155:1)
      --start-block <BLOCK>     Override start block
      --end-block <BLOCK>       Override end block
      --rpc <URLS>              Override RPC endpoints (comma-separated)
  -v, --verbose                 Show verbose progress output
      --watch                   Run continuously in daemon mode
      --interval <SECONDS>      Polling interval in watch mode [default: 60]
      --api-port <PORT>         Start HTTP API on port
      --host <HOST>             Bind the API to this host [default: 127.0.0.1]
      --add-address <ADDR>      Add address(es) to file and exit (repeatable)
      --remove-address <ADDR>   Remove address(es) from file and exit (repeatable)
      --add-chain <SPEC>        Add chain: NAME,CAIP2,RPC_URL1,RPC_URL2
      --remove-chain <NAME>     Remove chain from Config.toml by name
      --add-asset <SPEC>        Add asset: CHAIN_NAME,ASSET_NAME,CONTRACT,DECIMALS
      --remove-asset <SPEC>     Remove asset: CHAIN_NAME,ASSET_NAME
  -h, --help                    Print help
  -V, --version                 Print version
```

### Examples

```bash
# Single scan of Ethereum mainnet, output JSON to stdout
rustplorer -c Config.toml -a addresses.txt --network eip155:1

# Save results as CSV
rustplorer -c Config.toml -a addresses.txt -f csv -o deposits.csv

# Daemon mode: poll every 30s, serve API on port 8080
rustplorer -c Config.toml -a addresses.txt --watch --interval 30 --api-port 8080 -v

# Add a new chain via CLI (preserves comments in Config.toml)
rustplorer --add-chain "arbitrum,eip155:42161,https://arb1.arbitrum.io/rpc,https://arbitrum.drpc.org"

# Remove a chain
rustplorer --remove-chain polygon

# Add an ERC-20 token to an existing chain
rustplorer --add-asset "ethereum,DAI,0x6B175474E89094C44Da98b954EedeAC495271d0F,18"

# Add a tracked address
rustplorer -a addresses.txt --add-address "0x70997970c51812dc3a010c7d01b50e0d17dc79c8"
```

---

## HTTP API

When started with `--api-port`, rustplorer serves a REST API bound to `127.0.0.1` by default.

### Endpoints

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/` | Built-in dashboard (HTML) |
| `GET` | `/v1/deposits` | Recent deposits from in-memory ring buffer (cap 100) |
| `GET` | `/v1/addresses` | List tracked addresses |
| `POST` | `/v1/addresses` | Add addresses: `{"address": "0x..."}` or `{"addresses": ["0x1", "0x2"]}` |
| `DELETE` | `/v1/addresses/:addr` | Remove a tracked address |
| `GET` | `/v1/config` | Current configuration (nested structure) |
| `POST` | `/v1/chains` | Add a chain: `{"name": "arbitrum", "caip2": "eip155:42161", "rpc": ["https://..."]}` |
| `DELETE` | `/v1/chains/:name` | Remove a chain by name |
| `POST` | `/v1/assets` | Add an asset: `{"chain": "ethereum", "name": "DAI", "contract": "0x6B17...", "decimals": 18}` |
| `DELETE` | `/v1/assets/:chain/:asset` | Remove an asset |

### Output Examples

All JSON responses follow the JSON:API specification: every response is a top-level JSON object (never a bare array) with `data` and `meta` keys for success, or an `errors` array for failures. This allows future extensibility (pagination, hypermedia links) without breaking changes.

#### `GET /v1/deposits` — Recent Deposits

Returns up to 100 most recent deposits from the in-memory ring buffer, newest first.

```bash
curl http://127.0.0.1:3000/v1/deposits
```

```json
{
  "data": [
    {
      "chain": "ethereum",
      "asset": "USDC",
      "from_address": "0x3fC91A3afd70395Cd496C647d5a6CC9D4B2b7FAD",
      "to_address": "0x70997970c51812dc3a010c7d01b50e0d17dc79c8",
      "amount_raw": "50000000",
      "amount_clean": "50",
      "block_number": 19000123,
      "tx_hash": "0xabc123def456789012345678901234567890abcdef1234567890abcdef123456"
    },
    {
      "chain": "solana",
      "asset": "SOL_NATIVE",
      "from_address": "7EcDhSYGxXyscszYEp35KHN8vvw3svAuLKTzXwCFLtV",
      "to_address": "DRpbCBMxVnDK7maPMoGQfFiB4P4cByAHpLMkP1g8vAJw",
      "amount_raw": "1500000000",
      "amount_clean": "1.5",
      "block_number": 245678901,
      "tx_hash": "5UfDuX7EWsBzZcJvQeYq5tBKDvaR5LfrRHFFqWf6n3mQvQkBgF5rF1CCvY3qL1vz6HKqSLk3wRwYkY6vBvs3XVh"
    },
    {
      "chain": "bitcoin",
      "asset": "BTC_NATIVE",
      "from_address": "bc1qxy2kgdygjrsqtzq2n0yrf2493p83kkfjhx0wlh",
      "to_address": "bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq",
      "amount_raw": "12345678",
      "amount_clean": "0.12345678",
      "block_number": 831001,
      "tx_hash": "a1075db55d416d3ca199f55b6084e2115b9345e16c5cf302fc80e9d5fbf5d48d"
    },
    {
      "chain": "ethereum",
      "asset": "ETH_NATIVE",
      "from_address": "0x95222290DD7278Aa3Ddd389Cc1E1d165CC4BAfe5",
      "to_address": "0x70997970c51812dc3a010c7d01b50e0d17dc79c8",
      "amount_raw": "2500000000000000000",
      "amount_clean": "2.5",
      "block_number": 19000120,
      "tx_hash": "0xdef789abc012345678901234567890abcdef123456789012345678901234abcd"
    }
  ],
  "meta": {
    "total": 4
  }
}
```

#### `GET /v1/addresses` — List Tracked Addresses

Returns all currently tracked addresses, grouped by chain type where applicable.

```bash
curl http://127.0.0.1:3000/v1/addresses
```

```json
{
  "data": [
    "0x70997970c51812dc3a010c7d01b50e0d17dc79c8",
    "0x3c44cdddb6a900fa2b585dd299e03d12fa4293bc",
    "DRpbCBMxVnDK7maPMoGQfFiB4P4cByAHpLMkP1g8vAJw",
    "bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq"
  ],
  "meta": {
    "total": 4
  }
}
```

#### `POST /v1/addresses` — Add Addresses

Add one or more addresses to the tracker. Supports both single and batch modes. Returns `201 Created` on success.

**Single address:**

```bash
curl -X POST http://127.0.0.1:3000/v1/addresses \
  -H "Content-Type: application/json" \
  -d '{"address": "0x70997970c51812dc3a010c7d01b50e0d17dc79c8"}'
```

```json
{
  "data": {
    "added": 1
  },
  "meta": {
    "total": 5
  }
}
```

**Batch addresses:**

```bash
curl -X POST http://127.0.0.1:3000/v1/addresses \
  -H "Content-Type: application/json" \
  -d '{"addresses": ["0x70997970c51812dc3a010c7d01b50e0d17dc79c8", "0x3c44cdddb6a900fa2b585dd299e03d12fa4293bc"]}'
```

```json
{
  "data": {
    "added": 2
  },
  "meta": {
    "total": 6
  }
}
```

**Error — invalid request (400 Bad Request):**

```json
{
  "errors": [
    {
      "status": "400",
      "title": "Bad Request",
      "detail": "no addresses provided"
    }
  ]
}
```

#### `DELETE /v1/addresses/:addr` — Remove an Address

Removes a tracked address by its exact string.

```bash
curl -X DELETE http://127.0.0.1:3000/v1/addresses/0x70997970c51812dc3a010c7d01b50e0d17dc79c8
```

```json
{
  "data": {
    "removed": 1
  },
  "meta": {
    "total": 5
  }
}
```

#### `GET /v1/config` — Current Configuration

Returns the full runtime configuration in the same nested structure as `Config.toml`.

```bash
curl http://127.0.0.1:3000/v1/config
```

```json
{
  "data": {
    "chains": {
      "ethereum": {
        "caip2": "eip155:1",
        "start_block": 19000000,
        "end_block": 19000500,
        "rpc": [
          "https://ethereum.publicnode.com",
          "https://eth.drpc.org",
          "https://1rpc.io/eth"
        ],
        "assets": {
          "ETH_NATIVE": {
            "contract": "native",
            "decimals": 18
          },
          "USDC": {
            "contract": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
            "decimals": 6
          }
        }
      },
      "solana": {
        "caip2": "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp",
        "rpc": [
          "https://api.mainnet-beta.solana.com"
        ],
        "rpc_options": {
          "max_concurrent": 1,
          "delay_ms": 500
        },
        "assets": {
          "SOL_NATIVE": {
            "contract": "native",
            "decimals": 9
          },
          "USDC": {
            "contract": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
            "decimals": 6
          }
        }
      },
      "bitcoin": {
        "caip2": "bip122:000000000019d6689c085ae165831e93",
        "rpc": [
          "https://bitcoin-rpc.publicnode.com"
        ],
        "assets": {
          "BTC_NATIVE": {
            "contract": "native",
            "decimals": 8
          }
        }
      }
    }
  }
}
```

#### `POST /v1/chains` — Add a Chain

Adds a new chain to the configuration. The chain is persisted to `Config.toml` via `toml_edit` and hot-reloaded on the next watch cycle. Returns `201 Created` on success.

```bash
curl -X POST http://127.0.0.1:3000/v1/chains \
  -H "Content-Type: application/json" \
  -d '{"name": "arbitrum", "caip2": "eip155:42161", "rpc": ["https://arb1.arbitrum.io/rpc", "https://arbitrum.drpc.org"]}'
```

```json
{
  "data": {
    "name": "arbitrum",
    "caip2": "eip155:42161",
    "rpc": [
      "https://arb1.arbitrum.io/rpc",
      "https://arbitrum.drpc.org"
    ]
  }
}
```

**With optional fields (start_block, rpc_options):**

```bash
curl -X POST http://127.0.0.1:3000/v1/chains \
  -H "Content-Type: application/json" \
  -d '{
    "name": "polygon",
    "caip2": "eip155:137",
    "rpc": ["https://polygon-rpc.com", "https://rpc.ankr.com/polygon"],
    "start_block": 50000000,
    "rpc_options": {"max_concurrent": 3, "delay_ms": 100}
  }'
```

```json
{
  "data": {
    "name": "polygon",
    "caip2": "eip155:137",
    "rpc": [
      "https://polygon-rpc.com",
      "https://rpc.ankr.com/polygon"
    ]
  }
}
```

**Error — invalid CAIP-2 format (400 Bad Request):**

```json
{
  "errors": [
    {
      "status": "400",
      "title": "Bad Request",
      "detail": "invalid CAIP-2 format: invalid_namespace (expected namespace:reference)"
    }
  ]
}
```

**Error — chain already exists (409 Conflict):**

```json
{
  "errors": [
    {
      "status": "409",
      "title": "Conflict",
      "detail": "chain already exists: ethereum"
    }
  ]
}
```

#### `DELETE /v1/chains/:name` — Remove a Chain

Removes a chain and all its assets from the configuration.

```bash
curl -X DELETE http://127.0.0.1:3000/v1/chains/polygon
```

```json
{
  "data": {
    "removed": "polygon"
  },
  "meta": {
    "remaining_chains": ["ethereum", "solana", "bitcoin"]
  }
}
```

**Error — chain not found (404 Not Found):**

```json
{
  "errors": [
    {
      "status": "404",
      "title": "Not Found",
      "detail": "chain not found: avalanche"
    }
  ]
}
```

#### `POST /v1/assets` — Add an Asset

Adds a token asset to an existing chain. The `chain` must match a chain already present in the configuration. Returns `201 Created` on success.

**Add an ERC-20 token:**

```bash
curl -X POST http://127.0.0.1:3000/v1/assets \
  -H "Content-Type: application/json" \
  -d '{"chain": "ethereum", "name": "DAI", "contract": "0x6B175474E89094C44Da98b954EedeAC495271d0F", "decimals": 18}'
```

```json
{
  "data": {
    "chain": "ethereum",
    "name": "DAI",
    "contract": "0x6B175474E89094C44Da98b954EedeAC495271d0F",
    "decimals": 18
  }
}
```

**Add a native token:**

```bash
curl -X POST http://127.0.0.1:3000/v1/assets \
  -H "Content-Type: application/json" \
  -d '{"chain": "base", "name": "ETH_NATIVE", "contract": "native", "decimals": 18}'
```

```json
{
  "data": {
    "chain": "base",
    "name": "ETH_NATIVE",
    "contract": "native",
    "decimals": 18
  }
}
```

**Error — chain not found (404 Not Found):**

```json
{
  "errors": [
    {
      "status": "404",
      "title": "Not Found",
      "detail": "chain not found: avalanche"
    }
  ]
}
```

**Error — asset already exists (409 Conflict):**

```json
{
  "errors": [
    {
      "status": "409",
      "title": "Conflict",
      "detail": "asset already exists on chain ethereum: USDC"
    }
  ]
}
```

#### `DELETE /v1/assets/:chain/:asset` — Remove an Asset

Removes a token asset from a chain.

```bash
curl -X DELETE http://127.0.0.1:3000/v1/assets/ethereum/DAI
```

```json
{
  "data": {
    "removed": {
      "chain": "ethereum",
      "name": "DAI"
    }
  },
  "meta": {
    "remaining_assets_on_chain": ["ETH_NATIVE", "USDC"]
  }
}
```

**Error — chain not found (404 Not Found):**

```json
{
  "errors": [
    {
      "status": "404",
      "title": "Not Found",
      "detail": "chain not found: avalanche"
    }
  ]
}
```

**Error — asset not found (404 Not Found):**

```json
{
  "errors": [
    {
      "status": "404",
      "title": "Not Found",
      "detail": "asset not found on chain ethereum: UNI"
    }
  ]
}
```

#### `GET /` — Dashboard

Returns the built-in HTML dashboard. Not a JSON endpoint — serves a complete single-page application.

```bash
curl http://127.0.0.1:3000/
```

```
Content-Type: text/html; charset=utf-8

<!DOCTYPE html>
<html lang="en" data-theme="dark">
  <!-- Full dashboard SPA -->
</html>
```

All config mutations use `toml_edit` internally, so comments and formatting in `Config.toml` are preserved across API and CLI edits. Changes are hot-reloaded on the next watch cycle.

---

## Dashboard

The built-in dashboard at `http://localhost:3000/` provides a real-time UI for monitoring deposits, managing addresses, and configuring chains and assets.

### Features

- **Dark/light theme** with toggle (persisted in `localStorage`)
- **Address sidebar** — view and manage tracked addresses with one-click add/delete
- **Deposit feed** — live-updating deposit cards with chain icons, token amounts, and truncated addresses
- **Settings modal** — tabbed interface for managing chains and assets
- **Custom confirm modal** — no `window.confirm()` dialogs; clean, styled confirmation
- **Responsive** — mobile-friendly with tab-based navigation
- **Toast notifications** — success/error/info feedback for all actions
- **Status bar** — connection status, last poll time, chain count

### Chain Icons

The dashboard automatically maps chain names and CAIP-2 identifiers to icons and colors:

| Chain | Icon | Color |
|-------|------|-------|
| Ethereum | ETH | Indigo |
| Base | BASE | Blue |
| Polygon | POLY | Purple |
| Arbitrum | ARB | Cyan |
| Optimism | OP | Red |
| BSC | BSC | Amber |
| Solana | SOL | Green |
| Bitcoin | BTC | Amber |

---

## Architecture

### Data Flow

```
Config.toml                    addresses.txt
     │                              │
     │  toml_edit::de::from_str()   │  load_addresses()
     ▼                              ▼
AppConfig                       HashSet<String>
(chains: HashMap)               (validated EVM addrs)
     │                              │
     └──────────┬───────────────────┘
                │
                ▼
         run_indexer()
                │
    ┌───────────┼───────────┐
    │           │           │
    ▼           ▼           ▼
EvmScanner  SolScanner  BtcScanner
    │           │           │
    │  tx.send(DepositResult)
    │           │           │
    └───────────┼───────────┘
                │
      mpsc::channel(50_000)
                │
                ▼
         tokio::select! {
           recv  => deposits.push(d)
           ctrl_c => graceful shutdown
         }
                │
                ▼
        Vec<DepositResult>
           ┌────┴────┐
           │         │
     stdout/file   VecDeque (cap 100)
                   recent_deposits
                        │
                  GET /v1/deposits (O(1))
```

### DepositResult

Every detected deposit produces a `DepositResult`:

```rust
pub struct DepositResult {
    pub chain: String,         // Chain name from config (e.g. "ethereum")
    pub asset: String,         // Asset ticker (e.g. "USDC", "Native")
    pub from_address: String,  // Sender address
    pub to_address: String,    // Recipient (matched target)
    pub amount_raw: String,    // Raw amount (wei, lamports, sats)
    pub amount_clean: String,  // Human-readable (e.g. "50", "1.5")
    pub block_number: u64,     // Block/slot height
    pub tx_hash: String,       // Transaction hash
}
```

### RPC Error Handling

The `execute_rpc` function in `src/rpc.rs` implements exponential backoff retry across all configured RPC endpoints:

1. Try each URL in order for the current round
2. On 429 (rate limit) or 5xx: move to the next URL
3. On JSON-RPC error: log with `tracing::warn!` and try next URL
4. After exhausting all URLs: wait `500ms * 2^round` (capped at 10s)
5. Retry up to 3 rounds total
6. If all rounds fail: return `Err` — never silently default to 0

### Address Validation

EVM addresses (starting with `0x`/`0X`) are validated using `alloy_primitives::Address` to ensure cryptographic correctness. Invalid addresses are skipped with a `tracing::warn!` message. Valid addresses are stored in lowercase for consistent matching.

Solana and Bitcoin addresses are stored as-is since they have different encoding schemes (Base58 for Solana, Bech32/Base58 for Bitcoin).

### BTC Precision

Bitcoin amounts from the RPC come as JSON numbers (e.g. `0.12345678`). With `serde_json`'s `arbitrary_precision` feature enabled, the exact string representation is preserved — no IEEE-754 floating-point intermediate. The value is then parsed with `rust_decimal::Decimal::from_str()` and multiplied by `100_000_000` to get exact satoshis. This prevents precision loss for high-value transactions.

---

## Installation

### From Source

```bash
git clone https://github.com/maxylev/rustplorer.git
cd rustplorer
cargo install --path .
```

### From crates.io

```bash
cargo install rustplorer
```

### Prerequisites

- **Rust 1.95.0+** (edition 2024)

```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup default stable
rustc --version  # ensure 1.95.0+
```

### Release Profile

The `Cargo.toml` includes an optimized release profile:

```toml
[profile.release]
opt-level = 3
lto = "fat"
codegen-units = 1
strip = true
panic = "abort"
```

---

## Docker

### Build Image

```bash
docker build -t rustplorer .
```

### Run

```bash
docker run -d \
  --name rustplorer \
  -p 3000:3000 \
  -v $(pwd)/Config.toml:/app/Config.toml \
  -v $(pwd)/addresses.txt:/app/addresses.txt \
  rustplorer \
  --config /app/Config.toml \
  --addresses /app/addresses.txt \
  --watch \
  --api-port 3000 \
  --interval 60 \
  -v
```

### Docker Compose

```yaml
version: "3.8"
services:
  rustplorer:
    build: .
    ports:
      - "3000:3000"
    volumes:
      - ./Config.toml:/app/Config.toml
      - ./addresses.txt:/app/addresses.txt
    command: >
      --config /app/Config.toml
      --addresses /app/addresses.txt
      --watch
      --api-port 3000
      --interval 60
      -v
    restart: unless-stopped
```

---

## Testing

### Unit Tests

```bash
# Run all unit and mock tests
cargo test --all-targets

# Verbose output with tracing
RUST_LOG=debug cargo test --all-targets -- --nocapture

# Specific module
cargo test --lib format
```

Unit tests use `mockito` 1.7 for mock HTTP servers, covering ERC-20 detection, native deposit detection, batched ERC-20, RPC fallback, error propagation, Solana/BTC scanning, MPSC channel aggregation, config loading, and address validation.

### E2E Tests

See [`e2e.md`](e2e.md) for comprehensive end-to-end test scenarios covering all chains with local nodes (anvil, solana-test-validator, bitcoind in regtest mode).

---

## Dependencies

| Crate | Version | Purpose |
|-------|---------|---------|
| tokio | 1.52 | Async runtime (full features) |
| reqwest | 0.13 | HTTP client for JSON-RPC |
| axum | 0.8 | HTTP API server |
| serde | 1 | Serialization/deserialization |
| serde_json | 1 (arbitrary_precision) | Lossless JSON number parsing |
| toml_edit | 0.25 (serde) | Comment-preserving config mutation |
| futures | 0.3 | Stream utilities for concurrent RPC |
| hashbrown | 0.17 | High-performance HashSet for addresses |
| clap | 4 (derive) | CLI argument parsing |
| alloy-primitives | 1.6 | EVM address validation & EIP-55 |
| rust_decimal | 1.42 | Lossless BTC satoshi parsing |
| num-bigint | 0.4 | Arbitrary-precision integer for formatting |
| csv | 1 | CSV output format |
| hex | 0.4 | Hex encoding/decoding |
| tracing | 0.1 | Structured logging |
| tracing-subscriber | 0.3 (env-filter) | Log filtering via `RUST_LOG` |
| anyhow | 1 | Error handling |

### Dev Dependencies

| Crate | Version | Purpose |
|-------|---------|---------|
| mockito | 1.7 | Mock HTTP server for unit tests |
| tempfile | 3 | Temporary file creation in tests |

---

## Project Structure

```
rustplorer/
├── Cargo.toml              # Package manifest with latest deps
├── Config.example.toml     # Example configuration (nested format)
├── Dockerfile              # Multi-stage build (rust:1.95.0-slim)
├── README.md               # This file
├── e2e.md                  # Comprehensive E2E testing document
├── index.html              # Built-in dashboard UI
└── src/
    ├── main.rs             # Entry point, CLI, API server, daemon loop
    ├── lib.rs              # Config types, address loading, run_indexer()
    ├── evm.rs              # EVM scanner (batched eth_getLogs, native + ERC-20)
    ├── solana.rs           # Solana scanner (native + SPL tokens)
    ├── btc.rs              # Bitcoin scanner (native, lossless precision)
    ├── rpc.rs              # JSON-RPC executor with backoff retry
    └── format.rs           # Raw-to-human amount formatting (BigUint)
```

---

## Configuration Management

Rustplorer provides three ways to manage your configuration:

1. **Manual editing** — edit `Config.toml` directly with any text editor. Comments and formatting are preserved across reloads.

2. **CLI flags** — `--add-chain`, `--remove-chain`, `--add-asset`, `--remove-asset`, `--add-address`, `--remove-address` all modify the config file using `toml_edit`, which preserves comments and formatting.

3. **HTTP API** — `POST/DELETE /chains`, `POST/DELETE /assets`, `POST/DELETE /addresses` for runtime management. Changes are written to `Config.toml` via `toml_edit` and hot-reloaded on the next watch cycle.

All three methods are compatible — you can mix manual edits, CLI changes, and API calls without conflicts.

---

## Logging

Rustplorer uses structured logging via the `tracing` crate. Control log verbosity with the `RUST_LOG` environment variable:

```bash
# Default (info and above)
RUST_LOG=info rustplorer --watch --api-port 3000 -a addresses.txt

# Debug-level for the application only
RUST_LOG=rustplorer=debug rustplorer --watch --api-port 3000 -a addresses.txt

# Trace everything (very verbose)
RUST_LOG=trace rustplorer --watch --api-port 3000 -a addresses.txt

# Warn for the app, debug for the RPC module
RUST_LOG=rustplorer=warn,rustplorer::rpc=debug rustplorer --watch --api-port 3000 -a addresses.txt
```

---

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this project by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
