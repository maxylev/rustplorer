# rustplorer End-to-End Testing Document

> **Version:** 0.8.0
> **Last Updated:** 2026-05-21
> **Rust Toolchain:** 1.95.0, edition 2024
> **Scope:** Full E2E test coverage for all supported chains (EVM, Solana, Bitcoin), API endpoints, CLI management flags, daemon mode, MPSC channel aggregation, in-memory ring buffer, nested config structure (`[chains.NAME]`), `toml_edit` comment preservation, structured logging, and edge cases.

---

## Table of Contents

1. [Prerequisites](#1-prerequisites)
2. [Test Environment Architecture](#2-test-environment-architecture)
3. [Environment Setup](#3-environment-setup)
4. [Test Config](#4-test-config)
5. [Unit & Mock Tests](#5-unit--mock-tests)
6. [E2E Test Scenarios](#6-e2e-test-scenarios)
7. [Automated E2E Test Script](#7-automated-e2e-test-script)
8. [Docker Compose Setup](#8-docker-compose-setup)
9. [Known Limitations](#9-known-limitations)
10. [Troubleshooting](#10-troubleshooting)

---

## 1. Prerequisites

The following tools must be installed and available on `PATH` before running E2E tests.

| Tool | Minimum Version | Purpose | Install |
|------|----------------|---------|---------|
| **Docker** | 24.0+ | Running local chain containers | [docker.com](https://docs.docker.com/get-docker/) |
| **Docker Compose** | 2.20+ | Orchestrating multi-container test stack | Included with Docker Desktop |
| **Foundry** | latest | `anvil` (local EVM), `cast`, `forge` (contract deployment) | `curl -L https://foundry.paradigm.xyz \| bash && foundryup` |
| **Solana CLI** | 1.18+ | `solana-test-validator` (local Solana) | `sh -c "$(curl -sSfL https://release.anza.xyz/stable/install)"` |
| **bitcoind** | 24.0+ | Bitcoin Core in regtest mode (verbosity 3 required) | [bitcoincore.org](https://bitcoincore.org/en/download/) |
| **Rust toolchain** | 1.95.0+ (edition 2024) | Building rustplorer | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` |
| **curl** | any | Driving API requests and RPC calls | System package manager |
| **jq** | 1.6+ | Parsing JSON responses in test assertions | System package manager |
| **Playwright** | 1.50+ (optional) | Browser-based UI tests (scenarios 6.16, 6.17) | `npx playwright install` |

### 1.1 Verify Prerequisites

```bash
docker --version                  # Docker version 24.0+
docker compose version            # Docker Compose version 2.20+
anvil --version                   # foundry: ...
forge --version                   # foundry: ...
cast --version                    # foundry: ...
solana --version                  # solana-cli 1.18+
bitcoind --version                # Bitcoin Core 24.0+
rustc --version                   # 1.95.0 (aarch64-unknown-linux-gnu)
cargo --version                   # cargo 1.95.0
curl --version                    # curl 8+
jq --version                      # jq-1.6+
```

### 1.2 Key Dependency Versions (as of June 2026)

These are the dependency versions that rustplorer v0.8.0 is built against. Mismatches in the tokio, reqwest, or axum versions may cause runtime incompatibilities.

| Crate | Version | Purpose |
|-------|---------|---------|
| tokio | 1.52 | Async runtime (full features) |
| reqwest | 0.13 | HTTP client for JSON-RPC calls |
| axum | 0.8 | HTTP API server framework |
| hashbrown | 0.17 | High-performance HashSet for target addresses |
| alloy-primitives | 1.6 | EVM address validation & EIP-55 normalization |
| rust_decimal | 1.42 | Lossless BTC satoshi parsing |
| num-bigint | 0.4 | Arbitrary-precision integer for amount formatting |
| toml_edit | 0.25 | Comment-preserving Config.toml mutation |
| clap | 4 | CLI argument parsing (derive) |
| serde_json | 1 (arbitrary_precision) | Lossless JSON number parsing |
| serde | 1 | Serialization/deserialization |
| tracing | 0.1 | Structured logging |
| tracing-subscriber | 0.3 (env-filter) | Log filtering via RUST_LOG |
| futures | 0.3 | Stream utilities for concurrent RPC |
| csv | 1 | CSV output format |
| hex | 0.4 | Hex encoding/decoding |
| anyhow | 1 | Error handling |
| mockito | 1.7 | Mock HTTP server for unit tests |
| tempfile | 3 | Temporary file creation in tests |

### 1.3 v0.8.0 Breaking Changes from v0.7.0

| Change | v0.7.0 (Old) | v0.8.0 (New) |
|--------|-------------|-------------|
| Config structure | `[[chains]]` array + `[assets.X]` with `caip2` | `[chains.NAME]` nested with `[chains.NAME.assets.X]` |
| Asset `caip2` field | Required on every asset | Removed — inherited from parent chain |
| RPC options | `max_concurrent`, `rpc_delay_ms` at chain level | Grouped under `[chains.NAME.rpc_options]` |
| `DepositResult.chain` | Was CAIP-2 value (e.g., `"eip155:31337"`) | Now holds human-readable chain name (e.g., `"anvil"`); CAIP-2 moved to `ChainConfig.caip2` |
| `DepositResult.token` | Field name `token` | Renamed to `DepositResult.asset` |
| `toml_edit` | Not used | Used for comment-preserving TOML mutation |
| Chain management | No CLI/API support | CLI flags + API endpoints for add/remove chain/asset |

---

## 2. Test Environment Architecture

The E2E test environment spins up three local blockchain nodes, a rustplorer daemon, and a test harness that drives transactions and verifies deposit detection. The v0.8.0 architecture uses MPSC channels for non-blocking result aggregation and an in-memory ring buffer (VecDeque, cap 100) for the `/v1/deposits` API.

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                        E2E Test Environment (v0.8.0)                        │
│                                                                             │
│  ┌──────────────┐  ┌──────────────────┐  ┌──────────────────────┐          │
│  │   anvil      │  │  solana-test-    │  │     bitcoind         │          │
│  │  (EVM node)  │  │    validator     │  │   (regtest mode)     │          │
│  │              │  │                  │  │                      │          │
│  │  :8545       │  │  :8899           │  │  :18443              │          │
│  │  :8546 (WS)  │  │  :8900 (WS)     │  │                      │          │
│  └──────┬───────┘  └────────┬─────────┘  └──────────┬───────────┘          │
│         │                   │                       │                      │
│         │  JSON-RPC         │  JSON-RPC             │  JSON-RPC            │
│         │                   │                       │                      │
│  ┌──────┴───────────────────┴───────────────────────┴──────────────────┐   │
│  │                       rustplorer daemon (v0.8.0)                     │   │
│  │                                                                      │   │
│  │  ┌────────────────────────────────────────────────────────────────┐ │   │
│  │  │  Nested Config: [chains.anvil]  [chains.solana]  [chains.btc] │ │   │
│  │  │    .caip2          .caip2           .caip2                    │ │   │
│  │  │    .rpc[]          .rpc[]           .rpc[]                    │ │   │
│  │  │    .assets.ETH     .rpc_options     .assets.BTC_NATIVE       │ │   │
│  │  │    .assets.MTK     .assets.SOL                                │ │   │
│  │  │    .assets.TUSD                                               │ │   │
│  │  └────────────────────────────────────────────────────────────────┘ │   │
│  │                                                                      │   │
│  │  Scanner threads:  EVM · Solana · BTC                               │   │
│  │      │                                                               │   │
│  │      │  MPSC channel (cap 50,000)                                    │   │
│  │      │  "Do not communicate by sharing memory;                      │   │
│  │      │   share memory by communicating."                             │   │
│  │      ▼                                                               │   │
│  │  Receiver ──► Vec<DepositResult>                                     │   │
│  │                    │                                                  │   │
│  │                    ▼                                                  │   │
│  │  In-memory ring buffer: VecDeque<DepositResult> (cap 100)           │   │
│  │      ├── /v1/deposits endpoint reads from here (O(1), no disk I/O)    │   │
│  │      └── New deposits push_front; oldest pop_back at cap            │   │
│  │                                                                      │   │
│  │  API server:  127.0.0.1:3000 (localhost-only by default)           │   │
│  │  Config mgmt:  toml_edit (comment-preserving TOML mutation)        │   │
│  │  Logging:      tracing + tracing-subscriber (env-filter)           │   │
│  │  Shutdown:     tokio::select! { recv | ctrl_c() }                  │   │
│  └──────────────────────────────┬───────────────────────────────────────┘   │
│                                 │                                          │
│                        HTTP API (127.0.0.1:3000)                           │
│                                 │                                          │
│  ┌──────────────────────────────┴───────────────────────────────────────┐   │
│  │                          Test Harness (bash)                         │   │
│  │                                                                      │   │
│  │  • cast send — drives EVM transactions                               │   │
│  │  • solana transfer — drives Solana transactions                      │   │
│  │  • bitcoin-cli — drives Bitcoin transactions                         │   │
│  │  • curl — queries rustplorer API & verifies deposits                 │   │
│  │  • jq — asserts deposit fields                                       │   │
│  │  • date +%s%3N — measures API response latency                       │   │
│  └──────────────────────────────────────────────────────────────────────┘   │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

### 2.1 MPSC Channel Flow (v0.8.0)

Each scanner sends deposits through an `mpsc::Sender<DepositResult>` channel (cap 50,000). The receiver collects them in a `tokio::select!` block that also listens for `ctrl_c()` for graceful shutdown.

```
┌─────────────┐  ┌─────────────┐  ┌─────────────┐
│ EvmScanner  │  │ SolScanner  │  │ BtcScanner  │
│             │  │             │  │             │
│ tx.send(d1) │  │ tx.send(d2) │  │ tx.send(d3) │
│             │  │             │  │             │
│ chain       │  │ chain       │  │ chain       │
│ = "anvil"   │  │ = "solana"  │  │ = "bitcoin" │
└──────┬──────┘  └──────┬──────┘  └──────┬──────┘
       │                │                │
       └────────────────┼────────────────┘
                        │
              mpsc::channel(50_000)
                        │
                        ▼
              ┌─────────────────────────────┐
              │  tokio::select! {           │
              │    deposit = rx.recv() => { │
              │      deposits.push(d);      │
              │    }                        │
              │    _ = ctrl_c() => {        │
              │      graceful shutdown;     │
              │    }                        │
              │  }                          │
              └──────────┬──────────────────┘
                         │
                         ▼
              ┌──────────────────────────────┐
              │ VecDeque (cap 100)           │
              │ recent_deposits              │
              │ ├── push_front (newest)      │
              │ ├── pop_back (at cap=100)    │
              │ ├── /v1/deposits reads O(1)     │
              │ └── no disk I/O              │
              └──────────────────────────────┘
```

### 2.2 Port Map

| Service | Host | Port | Protocol |
|---------|------|------|----------|
| anvil | 127.0.0.1 | 8545 | HTTP JSON-RPC |
| anvil | 127.0.0.1 | 8546 | WebSocket |
| solana-test-validator | 127.0.0.1 | 8899 | HTTP JSON-RPC |
| solana-test-validator | 127.0.0.1 | 8900 | WebSocket |
| bitcoind | 127.0.0.1 | 18443 | HTTP JSON-RPC |
| rustplorer API | 127.0.0.1 | 3000 | HTTP REST |

### 2.3 Batched eth_getLogs (v0.8.0)

In v0.8.0, all contract addresses for a chain are collected from `self.assets` (chain-local, no `caip2` filter needed) and ONE `eth_getLogs` call is made per 200-block chunk:

```
v0.7.0 OLD (flat assets with caip2 filter):
  For each asset where asset.caip2 == chain.caip2:
    eth_getLogs(address=[MTK_addr])   ──► logs for MTK only
    eth_getLogs(address=[TUSD_addr])  ──► logs for TUSD only
    = N RPC calls per 200-block chunk (one per ERC-20 asset)

v0.8.0 NEW (nested assets, no caip2 filter):
  All assets in self.assets already belong to this chain:
    eth_getLogs(address=[MTK_addr, TUSD_addr, USDC_addr]) ──► all logs
    = 1 RPC call per 200-block chunk (N× fewer calls)
```

The key v0.8.0 insight: since assets are nested under `[chains.<name>.assets]`, the `scan_erc20()` method iterates `self.assets` directly — no `caip2` filtering required. Every asset in the map belongs to this chain.

### 2.4 Data Flow: Nested Config → DepositResult

```
Config.test.toml
  │
  │  toml_edit::de::from_str()
  ▼
AppConfig { chains: HashMap<String, ChainConfig> }
  │
  ├── chains["anvil"] ──── ChainConfig {
  │       caip2: "eip155:31337",
  │       rpc: ["http://127.0.0.1:8545"],
  │       assets: {
  │         "ETH_NATIVE": AssetConfig { contract: "native", decimals: 18 },
  │         "MTK":        AssetConfig { contract: "0xABC...", decimals: 6 },
  │       },
  │     }
  │
  ├── chains["solana"] ── ChainConfig {
  │       caip2: "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp",
  │       rpc: ["http://127.0.0.1:8899"],
  │       rpc_options: Some(RpcOptions { max_concurrent: 1, delay_ms: 200 }),
  │       assets: {
  │         "SOL_NATIVE": AssetConfig { contract: "native", decimals: 9 },
  │       },
  │     }
  │
  └── chains["bitcoin"] ─ ChainConfig {
          caip2: "bip122:000000000019d6689c085ae165831e93",
          rpc: ["http://rpcuser:rpcpassword@127.0.0.1:18443"],
          assets: {
            "BTC_NATIVE": AssetConfig { contract: "native", decimals: 8 },
          },
        }
  │
  │  Scanner: tx.send(DepositResult { chain, asset, ... })
  ▼
DepositResult {
  chain:        "anvil",                 // HashMap key from config
  asset:        "MTK",                   // HashMap key from assets
  from_address: "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266",
  to_address:   "0x3c44cdddb6a900fa2b585dd299e03d12fa4293bc",
  amount_raw:   "50000000",
  amount_clean: "50",
  block_number: 42,
  tx_hash:      "0xdef472...",
}
```

---

## 3. Environment Setup

### 3.1 Starting Local Chains

#### 3.1.1 Starting anvil (Local EVM)

anvil ships with 10 pre-funded accounts. The default accounts and private keys are deterministic, making them ideal for reproducible tests.

```bash
# Start anvil in the background with deterministic state
anvil \
  --host 127.0.0.1 \
  --port 8545 \
  --chain-id 31337 \
  --block-time 1 \
  &> /tmp/anvil.log &

ANVIL_PID=$!
echo "anvil started (PID: $ANVIL_PID)"

# Wait for anvil to be ready
for i in $(seq 1 30); do
  if cast block-number --rpc-url http://127.0.0.1:8545 &>/dev/null; then
    echo "anvil is ready"
    break
  fi
  sleep 1
done
```

**Default anvil accounts (used throughout tests):**

| # | Address | Private Key |
|---|---------|-------------|
| 0 | `0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266` | `0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80` |
| 1 | `0x70997970C51812dc3A010C7d01b50e0d17dc79C8` | `0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d` |
| 2 | `0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC` | `0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a` |

#### 3.1.2 Starting solana-test-validator (Local Solana)

Prefer `solana-test-validator` for repeatable local tests. If local Solana is unavailable, tests may be run against devnet by setting private keys and RPC URLs via environment variables in your shell; never commit funded private keys to this repository.

```bash
# Start the Solana test validator in the background
solana-test-validator \
  --rpc-port 8899 \
  --quiet \
  &> /tmp/solana-test-validator.log &

SOLANA_PID=$!
echo "solana-test-validator started (PID: $SOLANA_PID)"

# Wait for the validator to be ready
for i in $(seq 1 60); do
  if solana slot --url http://127.0.0.1:8899 &>/dev/null; then
    echo "solana-test-validator is ready"
    break
  fi
  sleep 2
done

# Airdrop SOL to the default identity for test transactions
solana airdrop 50 --url http://127.0.0.1:8899
```

> **Note:** Remove `--quiet` for verbose output when debugging Solana validator issues.

#### 3.1.3 Starting bitcoind in regtest mode (Local Bitcoin)

```bash
# Create a temporary Bitcoin data directory
BITCOIN_DIR=$(mktemp -d /tmp/bitcoin-regtest-XXXXXX)

# Write a minimal bitcoin.conf
cat > "$BITCOIN_DIR/bitcoin.conf" <<'EOF'
regtest=1
server=1
rpcuser=rpcuser
rpcpassword=rpcpassword
rpcport=18443
port=18444
fallbackfee=0.0001
EOF

# Start bitcoind in the background
bitcoind \
  -regtest \
  -datadir="$BITCOIN_DIR" \
  -daemon \
  -rpcuser=rpcuser \
  -rpcpassword=rpcpassword \
  -rpcport=18443 \
  -fallbackfee=0.0001

echo "bitcoind started (datadir: $BITCOIN_DIR)"

# Wait for bitcoind to be ready
for i in $(seq 1 30); do
  if bitcoin-cli -regtest -rpcuser=rpcuser -rpcpassword=rpcpassword -rpcport=18443 getblockcount &>/dev/null; then
    echo "bitcoind is ready"
    break
  fi
  sleep 1
done
```

**RPC credentials used in all tests:**

| Parameter | Value |
|-----------|-------|
| RPC URL | `http://rpcuser:rpcpassword@127.0.0.1:18443` |
| Username | `rpcuser` |
| Password | `rpcpassword` |

#### 3.1.4 Deploying a MockToken ERC-20 Contract on anvil

We use a minimal ERC-20 contract deployed with `forge create`. The contract has configurable decimals and mints an initial supply to the deployer.

**Step 1:** Create a temporary Forge project for the mock contract:

```bash
# Create a temporary forge project
MOCK_DIR=$(mktemp -d /tmp/mock-token-XXXXXX)
cd "$MOCK_DIR"
forge init --no-commit
```

**Step 2:** Write the MockToken contract (`src/MockToken.sol`):

```solidity
// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "@openzeppelin/contracts/token/ERC20/ERC20.sol";

contract MockToken is ERC20 {
    uint8 private _decimals;

    constructor(
        string memory name_,
        string memory symbol_,
        uint8 decimals_,
        uint256 initialSupply
    ) ERC20(name_, symbol_) {
        _decimals = decimals_;
        _mint(msg.sender, initialSupply);
    }

    function decimals() public view override returns (uint8) {
        return _decimals;
    }
}
```

**Step 3:** Install OpenZeppelin and deploy:

```bash
cd "$MOCK_DIR"
forge install OpenZeppelin/openzeppelin-contracts --no-commit

# Deploy MockToken with 6 decimals and 1,000,000 MTK initial supply
MOCK_TOKEN_ADDR=$(forge create \
  --rpc-url http://127.0.0.1:8545 \
  --private-key 0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80 \
  src/MockToken.sol:MockToken \
  --constructor-args "Mock Token" "MTK" 6 1000000000000 \
  --json | jq -r '.deployedTo')

echo "MockToken deployed at: $MOCK_TOKEN_ADDR"
```

> **Note:** `1000000000000` = 1,000,000 MTK with 6 decimals (1,000,000 × 10⁶).

To deploy a second token (for batched ERC-20 testing):

```bash
MOCK_TOKEN_B_ADDR=$(forge create \
  --rpc-url http://127.0.0.1:8545 \
  --private-key 0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80 \
  src/MockToken.sol:MockToken \
  --constructor-args "Test USD" "TUSD" 18 1000000000000000000000000 \
  --json | jq -r '.deployedTo')

echo "TestUSD deployed at: $MOCK_TOKEN_B_ADDR"
```

> **Note:** `1000000000000000000000000` = 1,000,000 TUSD with 18 decimals.

---

## 4. Test Config

### 4.1 Config.test.toml — Nested Format (v0.8.0)

Save as `Config.test.toml`. The comments in this file are important — they are used to verify that `toml_edit` preserves them during CLI and API mutations (scenarios 6.8 and 6.9).

The v0.8.0 config uses the **nested `[chains.NAME]`** format where assets are scoped under their parent chain, eliminating the need for a separate `[assets]` section with redundant `caip2` fields.

```toml
# ==========================================
# rustplorer E2E Test Configuration (v0.8.0)
# Local chains only — no external RPCs
# ==========================================

# --- EVM (anvil local) ---
[chains.anvil]
caip2 = "eip155:31337"
rpc = [
    "http://127.0.0.1:8545",
]
start_block = 0

  # Native ETH
  [chains.anvil.assets.ETH_NATIVE]
  contract = "native"
  decimals = 18

  # MockToken — Replace MOCK_TOKEN_ADDR with the actual deployed address
  [chains.anvil.assets.MTK]
  contract = "MOCK_TOKEN_ADDR"
  decimals = 6

  # TestUSD — Replace MOCK_TOKEN_B_ADDR with the actual deployed address
  [chains.anvil.assets.TUSD]
  contract = "MOCK_TOKEN_B_ADDR"
  decimals = 18

# --- Solana (local validator) ---
[chains.solana]
caip2 = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp"
rpc = [
    "http://127.0.0.1:8899",
]

  # Rate-limiting options for Solana (respectful public RPC usage)
  [chains.solana.rpc_options]
  max_concurrent = 1
  delay_ms = 200

  [chains.solana.assets.SOL_NATIVE]
  contract = "native"
  decimals = 9

# --- Bitcoin (regtest) ---
[chains.bitcoin]
caip2 = "bip122:000000000019d6689c085ae165831e93"
rpc = [
    "http://rpcuser:rpcpassword@127.0.0.1:18443",
]

  [chains.bitcoin.assets.BTC_NATIVE]
  contract = "native"
  decimals = 8
```

Before running tests, replace the placeholder contract addresses:

```bash
# Replace placeholders with actual deployed addresses
sed -i "s/MOCK_TOKEN_ADDR/${MOCK_TOKEN_ADDR}/g" Config.test.toml
sed -i "s/MOCK_TOKEN_B_ADDR/${MOCK_TOKEN_B_ADDR}/g" Config.test.toml
```

### 4.2 Test addresses.txt

Save as `addresses.test.txt`:

```
0x70997970c51812dc3a010c7d01b50e0d17dc79c8
0x3c44cdddb6a900fa2b585dd299e03d12fa4293bc
```

> These are anvil accounts #1 and #2 (lowercase for consistent matching). The `alloy-primitives` library validates and normalizes them to EIP-55 checksummed form internally, then stores in lowercase for consistent matching. For Solana and Bitcoin tests, additional addresses will be added dynamically via the API.

### 4.3 Key Differences: v0.7.0 vs v0.8.0 Config

| Feature | v0.7.0 (Old) | v0.8.0 (New) |
|---------|-------------|-------------|
| Chain definition | `[[chains]]` (array of tables) | `[chains.NAME]` (named table) |
| Chain name in config | Not stored — only `caip2` | HashMap key = `"anvil"`, `"solana"`, etc. |
| Asset definition | `[assets.MTK]` with `caip2 = "eip155:31337"` | `[chains.anvil.assets.MTK]` — no `caip2` |
| RPC options | Top-level `max_concurrent`, `rpc_delay_ms` | `[chains.solana.rpc_options]` sub-table |
| Comment preservation | N/A (no TOML mutation) | `toml_edit::DocumentMut` preserves comments |

---

## 5. Unit & Mock Tests

### 5.1 Running Unit Tests

The project includes unit tests in the `format` module and integration-style mock tests using `mockito` 1.7 (a mock HTTP server library). Run all tests with:

```bash
cargo test --all-targets
```

For verbose output (showing all tracing log lines):

```bash
RUST_LOG=debug cargo test --all-targets -- --nocapture
```

To run a specific test module:

```bash
cargo test --lib format
cargo test --lib evm
cargo test --lib btc
cargo test --lib solana
```

To run tests with a specific RUST_LOG filter:

```bash
RUST_LOG=rustplorer=warn cargo test --all-targets
```

### 5.2 What the Mock Tests Cover

The mock tests use `mockito` 1.7 to spin up a fake HTTP server that returns pre-canned JSON-RPC responses, allowing us to test the scanner logic without real blockchain nodes.

| Test Case | Description | Module |
|-----------|-------------|--------|
| **EVM ERC-20 deposit detection** | Mock `eth_getLogs` response containing a Transfer event to a target address. Verify the `DepositResult` has correct `asset`, `chain`, `amount_clean`, `from_address`, `to_address`, and `tx_hash`. | `evm` |
| **EVM native deposit detection** | Mock `eth_getBlockByNumber` response with a value-bearing transaction to a target address. Verify `asset = "Native"`, `chain = "anvil"`, `amount_clean` matches the formatted ETH value. | `evm` |
| **EVM batched ERC-20** | Mock a single `eth_getLogs` call with multiple contract addresses in the filter. Verify that ALL token deposits are correctly mapped from the response `log.address` field back to asset names. This validates the v0.8.0 nested-asset optimization where all assets in `self.assets` belong to the current chain. | `evm` |
| **No-match filtering** | Mock responses where no transactions or events involve target addresses. Verify an empty `Vec<DepositResult>` is returned via the MPSC channel. | `evm`, `solana`, `btc` |
| **RPC fallback on error** | Configure mockito to return a 429 on the first URL and a valid response on the second. Verify the scanner falls back and succeeds. | `rpc` |
| **RPC error propagation** | Mock a JSON-RPC error response (e.g., `{"error":{"code":-32603,"message":"..."}}`). Verify that `get_tip()` returns `Err` instead of silently defaulting to block 0. | `evm`, `solana`, `btc` |
| **Solana native deposit detection** | Mock `getBlock` response with `preBalances`/`postBalances` showing an increase for a target address. Verify `asset = "Native"`, `chain = "solana"`, `amount_clean = "2.5"` (2.5 SOL). | `solana` |
| **BTC native deposit detection** | Mock `getblockhash` and `getblock` (verbosity 3) responses with a vout paying a target address. Verify `asset = "Native"`, `chain = "bitcoin"`, and correct satoshi amount. | `btc` |
| **BTC precision (arbitrary_precision)** | Mock `getblock` with `value: 0.12345678` and verify that `serde_json` with `arbitrary_precision` + `rust_decimal` produces exactly 12,345,678 sats with no floating-point loss. | `btc` |
| **MPSC channel send** | Verify that scanner methods correctly send deposits through `mpsc::Sender` and that the receiver collects all items when the sender is dropped. | `lib` |
| **Config loading (nested)** | Write a temporary `Config.toml` with `[chains.anvil]` nested format. Verify `load_config()` (via `toml_edit::de::from_str`) parses all fields correctly, including nested `assets` and `rpc_options`. | `lib` |
| **Address loading** | Write a temporary `addresses.txt` with valid and invalid EVM addresses. Verify valid addresses are normalized to lowercase by `alloy-primitives`; invalid addresses are skipped with a `tracing::warn!`. | `lib` |
| **Format human-readable values** | Verify `format_to_human()` with hex inputs (`0xde0b6b3a7640000` → `1`), decimal inputs (`2500000000` → `2.5`), zero values, and edge cases. | `format` |
| **toml_edit chain mutation** | Create a `DocumentMut` from a TOML string with comments. Add and remove `[chains.X]` sections. Verify comments are preserved after mutation. | (in main.rs tests) |
| **toml_edit asset mutation** | Create a `DocumentMut`, add `[chains.anvil.assets.NEWTK]`, then remove it. Verify the nested structure and comments are preserved. | (in main.rs tests) |

### 5.3 Mock Test Pattern (mockito 1.7)

The following pattern demonstrates how mock tests are structured using `mockito` 1.7 with the async API:

```rust
#[cfg(test)]
mod tests {
    use mockito::{Server, Mock};
    use reqwest::Client;
    use std::collections::HashMap;
    use rustplorer::*;

    #[tokio::test]
    async fn test_evm_erc20_deposit_nested_config() {
        let mut server = Server::new_async().await;
        let mock = server.mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"jsonrpc":"2.0","id":1,"result":[{
                "address": "0xabc0000000000000000000000000000000000000",
                "topics": [
                    "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef",
                    "0x000000000000000000000000f39fd6e51aad88f6f4ce6ab8827279cfffb92266",
                    "0x00000000000000000000000070997970c51812dc3a010c7d01b50e0d17dc79c8"
                ],
                "data": "0x02faf080",
                "blockNumber": "0x2a",
                "transactionHash": "0xdef472..."
            }]}"#)
            .create_async()
            .await;

        let rpc_url = server.url();
        let (tx, mut rx) = tokio::sync::mpsc::channel(100);

        // Build nested assets map (v0.8.0 style — no caip2 on assets)
        let mut assets = HashMap::new();
        assets.insert("MTK".to_string(), AssetConfig {
            contract: "0xabc0000000000000000000000000000000000000".to_string(),
            decimals: 6,
        });

        let scanner = evm::EvmScanner {
            rpc_urls: vec![rpc_url],
            caip2: "eip155:31337".to_string(),
            name: "anvil".to_string(),  // NEW in v0.8.0
            assets,
            rpc_delay_ms: None,
            max_concurrent: 5,
        };

        let client = Client::new();
        let targets = Arc::new(HashSet::from_iter(vec![
            "0x70997970c51812dc3a010c7d01b50e0d17dc79c8".to_string()
        ]));

        scanner.scan(client, 42, 42, targets, tx).await.unwrap();

        // Verify the mock was hit exactly once
        mock.assert_async().await;

        // Verify deposits via the MPSC channel receiver
        let deposit = rx.recv().await.expect("should receive deposit");
        assert_eq!(deposit.asset, "MTK");
        assert_eq!(deposit.chain, "anvil");
        assert_eq!(deposit.amount_clean, "50");
    }
}
```

### 5.4 MPSC Channel Test Pattern

Testing with the MPSC channel requires dropping the sender after scan completion so the receiver can terminate:

```rust
#[tokio::test]
async fn test_mpsc_channel_aggregation() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(50_000);

    // Simulate scanner sending deposits
    tokio::spawn(async move {
        tx.send(DepositResult {
            chain: "anvil".into(),
            asset: "MTK".into(),
            from_address: "0xabc".into(),
            to_address: "0xdef".into(),
            amount_raw: "50000000".into(),
            amount_clean: "50".into(),
            block_number: 42,
            tx_hash: "0x123".into(),
        }).await.unwrap();
        // tx is dropped here when the task completes
    });

    // Collect all deposits
    let mut deposits = vec![];
    while let Some(d) = rx.recv().await {
        deposits.push(d);
    }
    // rx.recv() returns None after all senders are dropped

    assert_eq!(deposits.len(), 1);
    assert_eq!(deposits[0].chain, "anvil");
}
```

### 5.5 RPC Error Propagation Test

v0.8.0 explicitly propagates RPC errors instead of silently defaulting to block 0:

```rust
#[tokio::test]
async fn test_get_tip_rpc_error() {
    let mut server = Server::new_async().await;
    let mock = server.mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32603,"message":"Internal error"}}"#)
        .create_async()
        .await;

    let client = Client::new();
    let url = server.url();

    let result = EvmScanner::get_tip(&client, &[url]).await;
    assert!(result.is_err(), "Should return Err on RPC error, not silently default to 0");
}
```

### 5.6 Nested Config Loading Test

```rust
#[test]
fn test_load_nested_config() {
    let toml_str = r#"
[chains.anvil]
caip2 = "eip155:31337"
rpc = ["http://127.0.0.1:8545"]
start_block = 0

  [chains.anvil.assets.ETH_NATIVE]
  contract = "native"
  decimals = 18

  [chains.anvil.assets.MTK]
  contract = "0xabc"
  decimals = 6

[chains.solana]
caip2 = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp"
rpc = ["http://127.0.0.1:8899"]

  [chains.solana.rpc_options]
  max_concurrent = 1
  delay_ms = 200

  [chains.solana.assets.SOL_NATIVE]
  contract = "native"
  decimals = 9
"#;

    let config: AppConfig = toml_edit::de::from_str(toml_str).unwrap();

    // Verify chain names are HashMap keys
    assert!(config.chains.contains_key("anvil"));
    assert!(config.chains.contains_key("solana"));

    // Verify nested assets
    let anvil = &config.chains["anvil"];
    assert_eq!(anvil.caip2, "eip155:31337");
    assert!(anvil.assets.contains_key("ETH_NATIVE"));
    assert!(anvil.assets.contains_key("MTK"));
    assert_eq!(anvil.assets["MTK"].contract, "0xabc");
    assert_eq!(anvil.assets["MTK"].decimals, 6);
    // No caip2 on assets — inherited from parent chain

    // Verify rpc_options
    let solana = &config.chains["solana"];
    assert_eq!(solana.rpc_options.as_ref().unwrap().max_concurrent, Some(1));
    assert_eq!(solana.rpc_options.as_ref().unwrap().delay_ms, Some(200));
}
```

### 5.7 toml_edit Comment Preservation Test

```rust
#[test]
fn test_toml_edit_preserves_comments() {
    let toml_str = r#"
# Main config
[chains.anvil]
caip2 = "eip155:31337"
rpc = ["http://127.0.0.1:8545"]

  [chains.anvil.assets.ETH_NATIVE]
  contract = "native"
  decimals = 18
"#;

    let mut doc: DocumentMut = toml_str.parse().unwrap();

    // Add a new chain
    let mut new_chain = Table::new();
    new_chain.insert("caip2", toml_edit::value("eip155:1"));
    let mut rpc_arr = toml_edit::Array::new();
    rpc_arr.push("https://ethereum.publicnode.com");
    new_chain.insert("rpc", Item::Value(rpc_arr.into()));

    if !doc.contains_key("chains") {
        doc.insert("chains", Item::Table(Table::new()));
    }
    doc["chains"].as_table_mut().unwrap().insert("ethereum", Item::Table(new_chain));

    let result = doc.to_string();

    // Verify the original comment is preserved
    assert!(result.contains("# Main config"), "Comment should be preserved after mutation");
    // Verify the new chain was added
    assert!(result.contains("[chains.ethereum]"));
    assert!(result.contains("eip155:1"));
}
```

---

## 6. E2E Test Scenarios

Each scenario follows a strict **Setup → Action → Verify** pattern. All commands assume the local chains from [Section 3](#3-environment-setup) are running.

### 6.1 EVM Native ETH Deposit

**Objective:** Verify that rustplorer detects a native ETH transfer to a tracked address.

#### Setup

```bash
# Target address (anvil account #1)
TARGET="0x70997970c51812dc3a010c7d01b50e0d17dc79c8"

# Sender (anvil account #0)
SENDER_KEY="0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
```

#### Action

```bash
# Record the current block number before sending
BLOCK_BEFORE=$(cast block-number --rpc-url http://127.0.0.1:8545)

# Send 1 ETH from account #0 to the target
TX_HASH=$(cast send \
  --rpc-url http://127.0.0.1:8545 \
  --private-key "$SENDER_KEY" \
  "$TARGET" \
  --value 1ether \
  --json | jq -r '.transactionHash')

echo "Sent 1 ETH, tx_hash=$TX_HASH"

# Record the block number after
BLOCK_AFTER=$(cast block-number --rpc-url http://127.0.0.1:8545)
```

#### Verify

```bash
OUTPUT=$(cargo run -- \
  --config Config.test.toml \
  --addresses addresses.test.txt \
  --network eip155:31337 \
  --start-block "$BLOCK_BEFORE" \
  --end-block "$BLOCK_AFTER" \
  --format json 2>/dev/null)

# Assertions
echo "$OUTPUT" | jq -e '.[0].chain == "anvil"' > /dev/null && echo "PASS: chain" || echo "FAIL: chain"
echo "$OUTPUT" | jq -e '.[0].asset == "Native"' > /dev/null && echo "PASS: asset" || echo "FAIL: asset"
echo "$OUTPUT" | jq -e '.[0].amount_clean == "1"' > /dev/null && echo "PASS: amount" || echo "FAIL: amount"
echo "$OUTPUT" | jq -e '.[0].to_address == "'$TARGET'"' > /dev/null && echo "PASS: to_address" || echo "FAIL: to_address"
echo "$OUTPUT" | jq -e '.[0].tx_hash == "'$TX_HASH'"' > /dev/null && echo "PASS: tx_hash" || echo "FAIL: tx_hash"
echo "$OUTPUT" | jq -e '.[0].amount_raw == "0xde0b6b3a7640000"' > /dev/null && echo "PASS: raw_amount hex" || echo "FAIL: raw_amount hex"
```

---

### 6.2 EVM ERC-20 Token Deposit

**Objective:** Verify detection of an ERC-20 token transfer to a tracked address.

#### Setup

Deploy MockToken (see [Section 3.1.4](#314-deploying-a-mocktoken-erc-20-contract-on-anvil)) and update `Config.test.toml` with the deployed contract address under `[chains.anvil.assets.MTK]`.

```bash
# Target address (anvil account #2)
TARGET="0x3c44cdddb6a900fa2b585dd299e03d12fa4293bc"
SENDER_KEY="0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"

# The MockToken address from deployment
MTK_ADDR=$MOCK_TOKEN_ADDR   # Set during deployment
```

#### Action

```bash
BLOCK_BEFORE=$(cast block-number --rpc-url http://127.0.0.1:8545)

# Transfer 50 MTK (50 × 10⁶ = 50000000 raw units with 6 decimals)
TX_HASH=$(cast send \
  --rpc-url http://127.0.0.1:8545 \
  --private-key "$SENDER_KEY" \
  "$MTK_ADDR" \
  "transfer(address,uint256)" \
  "$TARGET" \
  50000000 \
  --json | jq -r '.transactionHash')

echo "Sent 50 MTK, tx_hash=$TX_HASH"

BLOCK_AFTER=$(cast block-number --rpc-url http://127.0.0.1:8545)
```

#### Verify

```bash
OUTPUT=$(cargo run -- \
  --config Config.test.toml \
  --addresses addresses.test.txt \
  --network eip155:31337 \
  --start-block "$BLOCK_BEFORE" \
  --end-block "$BLOCK_AFTER" \
  --format json 2>/dev/null)

# Find the MTK deposit in the output
MTK_DEPOSIT=$(echo "$OUTPUT" | jq '[.[] | select(.asset == "MTK")] | .[0]')

echo "$MTK_DEPOSIT" | jq -e '.asset == "MTK"' > /dev/null && echo "PASS: asset" || echo "FAIL: asset"
echo "$MTK_DEPOSIT" | jq -e '.chain == "anvil"' > /dev/null && echo "PASS: chain" || echo "FAIL: chain"
echo "$MTK_DEPOSIT" | jq -e '.amount_clean == "50"' > /dev/null && echo "PASS: amount" || echo "FAIL: amount"
echo "$MTK_DEPOSIT" | jq -e '.to_address == "'$TARGET'"' > /dev/null && echo "PASS: to_address" || echo "FAIL: to_address"
echo "$MTK_DEPOSIT" | jq -e '.amount_raw == "50000000"' > /dev/null && echo "PASS: raw_amount" || echo "FAIL: raw_amount"
```

---

### 6.3 EVM Batched ERC-20 (Multiple Tokens, validate ONE eth_getLogs call)

**Objective:** Verify that rustplorer detects deposits from multiple ERC-20 tokens in a single scan, validating the batched `eth_getLogs` optimization where ONE RPC call is made per block chunk for ALL token contracts. This is a critical v0.8.0 regression test.

In v0.8.0, all assets in `self.assets` already belong to the current chain (nested structure), so no `caip2` filtering is needed. The `scan_erc20()` method collects ALL non-native contract addresses into a single array and makes ONE `eth_getLogs` call.

#### Setup

Deploy two tokens: MockToken (MTK, 6 decimals) and TestUSD (TUSD, 18 decimals). Configure both in `Config.test.toml` under `[chains.anvil.assets.MTK]` and `[chains.anvil.assets.TUSD]`.

```bash
TARGET="0x3c44cdddb6a900fa2b585dd299e03d12fa4293bc"
SENDER_KEY="0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
```

#### Action

```bash
BLOCK_BEFORE=$(cast block-number --rpc-url http://127.0.0.1:8545)

# Transfer 50 MTK (6 decimals: 50 × 10⁶ = 50000000)
cast send \
  --rpc-url http://127.0.0.1:8545 \
  --private-key "$SENDER_KEY" \
  "$MTK_ADDR" \
  "transfer(address,uint256)" \
  "$TARGET" \
  50000000

# Transfer 25 TUSD (18 decimals: 25 × 10¹⁸ = 25000000000000000000)
cast send \
  --rpc-url http://127.0.0.1:8545 \
  --private-key "$SENDER_KEY" \
  "$TUSD_ADDR" \
  "transfer(address,uint256)" \
  "$TARGET" \
  25000000000000000000

BLOCK_AFTER=$(cast block-number --rpc-url http://127.0.0.1:8545)
```

#### Verify

```bash
OUTPUT=$(cargo run -- \
  --config Config.test.toml \
  --addresses addresses.test.txt \
  --network eip155:31337 \
  --start-block "$BLOCK_BEFORE" \
  --end-block "$BLOCK_AFTER" \
  --format json 2>/dev/null)

# Verify BOTH deposits are detected
MTK_COUNT=$(echo "$OUTPUT" | jq '[.[] | select(.asset == "MTK")] | length')
TUSD_COUNT=$(echo "$OUTPUT" | jq '[.[] | select(.asset == "TUSD")] | length')

[ "$MTK_COUNT" -ge 1 ] && echo "PASS: MTK detected" || echo "FAIL: MTK not detected"
[ "$TUSD_COUNT" -ge 1 ] && echo "PASS: TUSD detected" || echo "FAIL: TUSD not detected"

# Verify correct amounts
echo "$OUTPUT" | jq -e '[.[] | select(.asset == "MTK")][0].amount_clean == "50"' > /dev/null && echo "PASS: MTK amount" || echo "FAIL: MTK amount"
echo "$OUTPUT" | jq -e '[.[] | select(.asset == "TUSD")][0].amount_clean == "25"' > /dev/null && echo "PASS: TUSD amount" || echo "FAIL: TUSD amount"

# Verify chain is present (v0.8.0)
echo "$OUTPUT" | jq -e '[.[] | select(.asset == "MTK")][0].chain == "anvil"' > /dev/null && echo "PASS: chain" || echo "FAIL: chain"
```

#### Verify batched RPC behavior (tracing check)

Enable verbose tracing and check that only ONE `eth_getLogs` call was made per block chunk for all tokens combined:

```bash
RUST_LOG=debug cargo run -- \
  --config Config.test.toml \
  --addresses addresses.test.txt \
  --network eip155:31337 \
  --start-block "$BLOCK_BEFORE" \
  --end-block "$BLOCK_AFTER" \
  --format json 2>&1 | tee /tmp/batch-trace.log

# Count eth_getLogs calls — should be 1 per 200-block chunk, NOT 2+
LOG_CALLS=$(rg -c "eth_getLogs" /tmp/batch-trace.log || echo 0)
echo "eth_getLogs calls in trace: $LOG_CALLS (should be minimal, NOT 2+ per chunk)"
```

---

### 6.4 EVM Auto End Block Detection

**Objective:** Verify that omitting `end_block` causes rustplorer to auto-detect the chain tip via `eth_blockNumber`.

#### Setup

```bash
TARGET="0x70997970c51812dc3a010c7d01b50e0d17dc79c8"
SENDER_KEY="0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
```

#### Action

```bash
# Send a transaction to advance the chain
cast send \
  --rpc-url http://127.0.0.1:8545 \
  --private-key "$SENDER_KEY" \
  "$TARGET" \
  --value 0.5ether

# Record the tip
TIP=$(cast block-number --rpc-url http://127.0.0.1:8545)
```

#### Verify

Create a temporary config that omits `end_block` (v0.8.0 nested format):

```bash
cat > Config.auto_end.toml <<'EOF'
[chains.anvil]
caip2 = "eip155:31337"
rpc = ["http://127.0.0.1:8545"]
start_block = 0
# end_block intentionally omitted — auto-detect chain tip

  [chains.anvil.assets.ETH_NATIVE]
  contract = "native"
  decimals = 18
EOF

# Run rustplorer — should auto-detect the tip
OUTPUT=$(cargo run -- \
  --config Config.auto_end.toml \
  --addresses addresses.test.txt \
  --format json 2>/dev/null)

# Verify deposits were found (proving it scanned up to the tip)
DEPOSIT_COUNT=$(echo "$OUTPUT" | jq 'length')
[ "$DEPOSIT_COUNT" -gt 0 ] && echo "PASS: auto end_block detection" || echo "FAIL: no deposits found"

# Verify chain is "anvil" (from the HashMap key)
echo "$OUTPUT" | jq -e '.[0].chain == "anvil"' > /dev/null && echo "PASS: chain" || echo "FAIL: chain"
```

---

### 6.5 Solana Native SOL Deposit

**Objective:** Verify detection of a native SOL transfer to a tracked address.

#### Setup

```bash
# Generate a new keypair for the test target
TARGET_KEY=$(solana-keygen new --no-bip39-passphrase --force --outfile /tmp/sol-target.json 2>/dev/null | grep "pubkey:" | awk '{print $2}')
echo "Solana target: $TARGET_KEY"

# Add the target to addresses file
echo "$TARGET_KEY" >> addresses.test.txt

# Default account (funded by airdrop)
DEFAULT_KEY="$HOME/.config/solana/id.json"
```

#### Action

```bash
# Airdrop 10 SOL to the default account
solana airdrop 10 --url http://127.0.0.1:8899

# Record the current slot
SLOT_BEFORE=$(solana slot --url http://127.0.0.1:8899)

# Transfer 2.5 SOL to the target
solana transfer \
  --url http://127.0.0.1:8899 \
  --allow-unfunded-recipient \
  "$TARGET_KEY" \
  2.5

# Wait for finalization
sleep 2

SLOT_AFTER=$(solana slot --url http://127.0.0.1:8899)
```

#### Verify

```bash
OUTPUT=$(cargo run -- \
  --config Config.test.toml \
  --addresses addresses.test.txt \
  --network "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp" \
  --start-block "$SLOT_BEFORE" \
  --end-block "$SLOT_AFTER" \
  --format json 2>/dev/null)

SOL_DEPOSIT=$(echo "$OUTPUT" | jq '[.[] | select(.asset == "Native")] | .[0]')

echo "$SOL_DEPOSIT" | jq -e '.chain == "solana"' > /dev/null && echo "PASS: chain" || echo "FAIL: chain"
echo "$SOL_DEPOSIT" | jq -e '.asset == "Native"' > /dev/null && echo "PASS: asset" || echo "FAIL: asset"
echo "$SOL_DEPOSIT" | jq -e '.amount_clean == "2.5"' > /dev/null && echo "PASS: amount" || echo "FAIL: amount"
echo "$SOL_DEPOSIT" | jq -e '.to_address == "'$TARGET_KEY'"' > /dev/null && echo "PASS: to_address" || echo "FAIL: to_address"
```

---

### 6.6 Bitcoin Native BTC Deposit

**Objective:** Verify detection of a native BTC transfer to a tracked address.

#### Setup

```bash
# Create a new Bitcoin address for the target
TARGET_BTC_ADDR=$(bitcoin-cli -regtest -rpcuser=rpcuser -rpcpassword=rpcpassword -rpcport=18443 getnewaddress)
echo "BTC target: $TARGET_BTC_ADDR"

# Add the target to addresses file
echo "$TARGET_BTC_ADDR" >> addresses.test.txt

# Generate a wallet address for mining rewards
MINER_ADDR=$(bitcoin-cli -regtest -rpcuser=rpcuser -rpcpassword=rpcpassword -rpcport=18443 getnewaddress "miner")
```

#### Action

```bash
# Mine 101 blocks to mature the coinbase (Bitcoin requires 100 confirmations)
bitcoin-cli -regtest -rpcuser=rpcuser -rpcpassword=rpcpassword -rpcport=18443 generatetoaddress 101 "$MINER_ADDR"

# Record the current block count
BLOCK_BEFORE=$(bitcoin-cli -regtest -rpcuser=rpcuser -rpcpassword=rpcpassword -rpcport=18443 getblockcount)

# Send 1.5 BTC to the target
TX_ID=$(bitcoin-cli -regtest -rpcuser=rpcuser -rpcpassword=rpcpassword -rpcport=18443 sendtoaddress "$TARGET_BTC_ADDR" 1.5)
echo "BTC txid: $TX_ID"

# Mine a confirmation block
bitcoin-cli -regtest -rpcuser=rpcuser -rpcpassword=rpcpassword -rpcport=18443 generatetoaddress 1 "$MINER_ADDR"

BLOCK_AFTER=$(bitcoin-cli -regtest -rpcuser=rpcuser -rpcpassword=rpcpassword -rpcport=18443 getblockcount)
```

#### Verify

```bash
OUTPUT=$(cargo run -- \
  --config Config.test.toml \
  --addresses addresses.test.txt \
  --network "bip122:000000000019d6689c085ae165831e93" \
  --start-block "$BLOCK_BEFORE" \
  --end-block "$BLOCK_AFTER" \
  --format json 2>/dev/null)

BTC_DEPOSIT=$(echo "$OUTPUT" | jq '[.[] | select(.asset == "Native")] | .[0]')

echo "$BTC_DEPOSIT" | jq -e '.chain == "bitcoin"' > /dev/null && echo "PASS: chain" || echo "FAIL: chain"
echo "$BTC_DEPOSIT" | jq -e '.asset == "Native"' > /dev/null && echo "PASS: asset" || echo "FAIL: asset"
# 1.5 BTC = 150,000,000 satoshis
echo "$BTC_DEPOSIT" | jq -e '.amount_raw == "150000000"' > /dev/null && echo "PASS: raw amount" || echo "FAIL: raw amount"
echo "$BTC_DEPOSIT" | jq -e '.amount_clean == "1.5"' > /dev/null && echo "PASS: clean amount" || echo "FAIL: clean amount"
```

---

### 6.7 Bitcoin Precision Test (0.12345678 BTC → exactly 12,345,678 sats)

**Objective:** Verify that precise BTC amounts are handled without floating-point precision loss. The `btc.rs` module uses `serde_json` with `arbitrary_precision` and `rust_decimal` to avoid IEEE-754 pitfalls.

This is a critical regression test. A naive `f64` parse of `0.12345678` produces `0.12345677999999999` due to floating-point representation, which would yield `12,345,677` sats instead of the correct `12,345,678`. The `arbitrary_precision` feature in `serde_json` preserves the exact decimal string, and `rust_decimal::Decimal` performs lossless multiplication by 100,000,000.

#### Setup

```bash
TARGET_BTC_ADDR=$(bitcoin-cli -regtest -rpcuser=rpcuser -rpcpassword=rpcpassword -rpcport=18443 getnewaddress "precision")
echo "$TARGET_BTC_ADDR" >> addresses.test.txt

MINER_ADDR=$(bitcoin-cli -regtest -rpcuser=rpcuser -rpcpassword=rpcpassword -rpcport=18443 getnewaddress "miner2")
bitcoin-cli -regtest -rpcuser=rpcuser -rpcpassword=rpcpassword -rpcport=18443 generatetoaddress 101 "$MINER_ADDR"
```

#### Action

```bash
BLOCK_BEFORE=$(bitcoin-cli -regtest -rpcuser=rpcuser -rpcpassword=rpcpassword -rpcport=18443 getblockcount)

# Send a precise amount: 0.12345678 BTC
TX_ID=$(bitcoin-cli -regtest -rpcuser=rpcuser -rpcpassword=rpcpassword -rpcport=18443 sendtoaddress "$TARGET_BTC_ADDR" 0.12345678)

# Mine a confirmation block
bitcoin-cli -regtest -rpcuser=rpcuser -rpcpassword=rpcpassword -rpcport=18443 generatetoaddress 1 "$MINER_ADDR"

BLOCK_AFTER=$(bitcoin-cli -regtest -rpcuser=rpcuser -rpcpassword=rpcpassword -rpcport=18443 getblockcount)
```

#### Verify

```bash
OUTPUT=$(cargo run -- \
  --config Config.test.toml \
  --addresses addresses.test.txt \
  --network "bip122:000000000019d6689c085ae165831e93" \
  --start-block "$BLOCK_BEFORE" \
  --end-block "$BLOCK_AFTER" \
  --format json 2>/dev/null)

BTC_DEPOSIT=$(echo "$OUTPUT" | jq '[.[] | select(.to_address == "'$TARGET_BTC_ADDR'")] | .[0]')

# 0.12345678 BTC = exactly 12,345,678 satoshis (no floating-point loss)
echo "$BTC_DEPOSIT" | jq -e '.amount_raw == "12345678"' > /dev/null && echo "PASS: precision raw" || echo "FAIL: precision raw"
echo "$BTC_DEPOSIT" | jq -e '.amount_clean == "0.12345678"' > /dev/null && echo "PASS: precision clean" || echo "FAIL: precision clean"
echo "$BTC_DEPOSIT" | jq -e '.chain == "bitcoin"' > /dev/null && echo "PASS: chain" || echo "FAIL: chain"
```

**Why this test matters:** Without `serde_json`'s `arbitrary_precision` feature, the JSON value `0.12345678` would be parsed as `f64` → `0.12345677999999999`, and `0.12345677999999999 × 100_000_000 = 12,345,677.999999999` → truncated to `12,345,677` sats. The `arbitrary_precision` + `rust_decimal` pipeline preserves the exact value: `"0.12345678"` → `Decimal(12345678, 8)` → `12,345,678` sats. The `btc.rs` module extracts the exact string representation using `vout["value"].as_number().map(|n| n.to_string())` — completely bypassing IEEE-754.

---

### 6.8 API — Add/Remove Chain via API (toml_edit comment preservation)

**Objective:** Verify that the `POST /v1/chains` and `DELETE /v1/chains/:name` API endpoints work correctly and that `toml_edit` preserves comments in `Config.toml`.

#### Setup

```bash
# Start rustplorer in daemon mode with API
cargo run -- \
  --config Config.test.toml \
  --addresses addresses.test.txt \
  --api-port 3000 \
  --watch \
  --interval 10 \
  &> /tmp/rustplorer-api.log &

RP_PID=$!
sleep 3

# Verify the API is responding
curl -s http://127.0.0.1:3000/v1/config | jq '.chains | keys'
```

#### Action: Add chain

```bash
# Add a new chain via API
curl -s -X POST http://127.0.0.1:3000/v1/chains \
  -H "Content-Type: application/json" \
  -d '{
    "name": "polygon",
    "caip2": "eip155:137",
    "rpc": ["https://polygon-rpc.com", "https://rpc.ankr.com/polygon"],
    "start_block": 50000000
  }' | jq .

# Expected response: "Chain 'polygon' added"
```

#### Verify: Chain added

```bash
# Verify the chain appears in the config
CONFIG=$(curl -s http://127.0.0.1:3000/v1/config)
echo "$CONFIG" | jq -e '.data.chains.polygon.caip2 == "eip155:137"' > /dev/null && echo "PASS: chain added" || echo "FAIL: chain not added"
echo "$CONFIG" | jq -e '.data.chains.polygon.rpc | length == 2' > /dev/null && echo "PASS: rpc count" || echo "FAIL: rpc count"
echo "$CONFIG" | jq -e '.data.chains.polygon.start_block == 50000000' > /dev/null && echo "PASS: start_block" || echo "FAIL: start_block"

# Verify comments are preserved in the TOML file
rg "# Main config" Config.test.toml && echo "PASS: comment preserved" || echo "FAIL: comment lost"
rg "# --- EVM" Config.test.toml && echo "PASS: section comment preserved" || echo "FAIL: section comment lost"

# Verify the new chain is in the TOML
rg '\[chains.polygon\]' Config.test.toml && echo "PASS: TOML chain entry" || echo "FAIL: TOML chain entry missing"
```

#### Action: Remove chain

```bash
# Remove the polygon chain via API
curl -s -X DELETE http://127.0.0.1:3000/v1/chains/polygon | jq .

# Expected response: "Chain 'polygon' removed"
```

#### Verify: Chain removed

```bash
# Verify the chain is gone from the config
CONFIG=$(curl -s http://127.0.0.1:3000/v1/config)
echo "$CONFIG" | jq -e '.data.chains.polygon == null' > /dev/null && echo "PASS: chain removed" || echo "FAIL: chain still present"

# Verify comments are STILL preserved after removal
rg "# Main config" Config.test.toml && echo "PASS: comment preserved after delete" || echo "FAIL: comment lost after delete"

# Verify original chains are untouched
echo "$CONFIG" | jq -e '.data.chains.anvil.caip2 == "eip155:31337"' > /dev/null && echo "PASS: anvil intact" || echo "FAIL: anvil corrupted"
echo "$CONFIG" | jq -e '.data.chains.solana.caip2 == "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp"' > /dev/null && echo "PASS: solana intact" || echo "FAIL: solana corrupted"
echo "$CONFIG" | jq -e '.data.chains.bitcoin.caip2 == "bip122:000000000019d6689c085ae165831e93"' > /dev/null && echo "PASS: bitcoin intact" || echo "FAIL: bitcoin corrupted"

# Cleanup
kill $RP_PID 2>/dev/null
```

---

### 6.9 API — Add/Remove Asset via API (nested under chain)

**Objective:** Verify that the `POST /v1/assets` and `DELETE /v1/assets/:chain/:asset` API endpoints correctly add and remove assets nested under their parent chain.

#### Setup

```bash
# Start rustplorer in daemon mode with API
cargo run -- \
  --config Config.test.toml \
  --addresses addresses.test.txt \
  --api-port 3000 \
  --watch \
  --interval 10 \
  &> /tmp/rustplorer-api.log &

RP_PID=$!
sleep 3
```

#### Action: Add asset

```bash
# Add a new ERC-20 asset to the anvil chain
curl -s -X POST http://127.0.0.1:3000/v1/assets \
  -H "Content-Type: application/json" \
  -d '{
    "chain": "anvil",
    "name": "USDC",
    "contract": "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
    "decimals": 6
  }' | jq .

# Expected response: "Asset 'USDC' added to chain 'anvil'"
```

#### Verify: Asset added under chain

```bash
# Verify the asset appears under chains.anvil.assets
CONFIG=$(curl -s http://127.0.0.1:3000/v1/config)
echo "$CONFIG" | jq -e '.data.chains.anvil.assets.USDC.contract == "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"' > /dev/null && echo "PASS: asset contract" || echo "FAIL: asset contract"
echo "$CONFIG" | jq -e '.data.chains.anvil.assets.USDC.decimals == 6' > /dev/null && echo "PASS: asset decimals" || echo "FAIL: asset decimals"

# Verify no caip2 field on the asset (v0.8.0 — inherited from chain)
echo "$CONFIG" | jq -e '.data.chains.anvil.assets.USDC | has("caip2") | not' > /dev/null && echo "PASS: no caip2 on asset" || echo "FAIL: asset has caip2"

# Verify existing assets are untouched
echo "$CONFIG" | jq -e '.data.chains.anvil.assets.ETH_NATIVE.contract == "native"' > /dev/null && echo "PASS: ETH_NATIVE intact" || echo "FAIL: ETH_NATIVE corrupted"
echo "$CONFIG" | jq -e '.data.chains.anvil.assets.MTK.decimals == 6' > /dev/null && echo "PASS: MTK intact" || echo "FAIL: MTK corrupted"

# Verify the asset is in the TOML under [chains.anvil.assets.USDC]
rg '\[chains.anvil.assets.USDC\]' Config.test.toml && echo "PASS: TOML nested asset entry" || echo "FAIL: TOML nested asset missing"
```

#### Action: Remove asset

```bash
# Remove the USDC asset from the anvil chain
curl -s -X DELETE http://127.0.0.1:3000/v1/assets/anvil/USDC | jq .

# Expected response: "Asset 'USDC' removed from chain 'anvil'"
```

#### Verify: Asset removed

```bash
CONFIG=$(curl -s http://127.0.0.1:3000/v1/config)
echo "$CONFIG" | jq -e '.data.chains.anvil.assets.USDC == null' > /dev/null && echo "PASS: asset removed" || echo "FAIL: asset still present"

# Verify other assets under the same chain are untouched
echo "$CONFIG" | jq -e '.data.chains.anvil.assets.ETH_NATIVE.contract == "native"' > /dev/null && echo "PASS: ETH_NATIVE still present" || echo "FAIL: ETH_NATIVE lost"
echo "$CONFIG" | jq -e '.data.chains.anvil.assets.MTK.decimals == 6' > /dev/null && echo "PASS: MTK still present" || echo "FAIL: MTK lost"

# Verify comments preserved
rg "# --- EVM" Config.test.toml && echo "PASS: comments preserved after asset mutation" || echo "FAIL: comments lost"

# Cleanup
kill $RP_PID 2>/dev/null
```

---

### 6.10 API — Ring Buffer `/v1/deposits` Instant Response

**Objective:** Verify that the `/v1/deposits` API endpoint reads from the in-memory ring buffer (VecDeque, cap 100) and responds instantly without disk I/O.

#### Setup

```bash
# Start rustplorer in daemon mode with API
cargo run -- \
  --config Config.test.toml \
  --addresses addresses.test.txt \
  --api-port 3000 \
  --watch \
  --interval 5 \
  --verbose \
  &> /tmp/rustplorer-ringbuffer.log &

RP_PID=$!
sleep 3

# Generate some deposits by sending ETH
SENDER_KEY="0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
TARGET="0x70997970c51812dc3a010c7d01b50e0d17dc79c8"

cast send --rpc-url http://127.0.0.1:8545 --private-key "$SENDER_KEY" "$TARGET" --value 1ether
cast send --rpc-url http://127.0.0.1:8545 --private-key "$SENDER_KEY" "$TARGET" --value 0.5ether

# Wait for a scan cycle to pick up the deposits
sleep 8
```

#### Action

```bash
# Query the deposits endpoint and measure response time
START_MS=$(date +%s%3N)
DEPOSITS=$(curl -s http://127.0.0.1:3000/v1/deposits)
END_MS=$(date +%s%3N)

ELAPSED_MS=$((END_MS - START_MS))
echo "Response time: ${ELAPSED_MS}ms"
```

#### Verify

```bash
# Verify the deposits endpoint returns data
DEPOSIT_COUNT=$(echo "$DEPOSITS" | jq '.data | length')
[ "$DEPOSIT_COUNT" -gt 0 ] && echo "PASS: deposits returned ($DEPOSIT_COUNT)" || echo "FAIL: no deposits"

# Verify response time is fast (ring buffer read, no disk I/O)
[ "$ELAPSED_MS" -lt 100 ] && echo "PASS: fast response (${ELAPSED_MS}ms < 100ms)" || echo "WARN: slow response (${ELAPSED_MS}ms >= 100ms)"

# Verify deposit structure
echo "$DEPOSITS" | jq -e '.data[0].chain != null' > /dev/null && echo "PASS: chain present" || echo "FAIL: chain missing"
echo "$DEPOSITS" | jq -e '.data[0].asset != null' > /dev/null && echo "PASS: asset present" || echo "FAIL: asset missing"
echo "$DEPOSITS" | jq -e '.data[0].amount_clean != null' > /dev/null && echo "PASS: amount_clean present" || echo "FAIL: amount_clean missing"
echo "$DEPOSITS" | jq -e '.meta.total > 0' > /dev/null && echo "PASS: meta.total present" || echo "FAIL: meta.total missing"

# Cleanup
kill $RP_PID 2>/dev/null
```

---

### 6.11 API — Address Management (Add/Remove via API)

**Objective:** Verify that the `/v1/addresses` API endpoints correctly manage the tracked address file.

#### Setup

```bash
# Start rustplorer in daemon mode with API
cargo run -- \
  --config Config.test.toml \
  --addresses addresses.test.txt \
  --api-port 3000 \
  --watch \
  --interval 10 \
  &> /tmp/rustplorer-addr.log &

RP_PID=$!
sleep 3
```

#### Action: List addresses

```bash
# List current addresses
curl -s http://127.0.0.1:3000/v1/addresses | jq .
# Should return the two addresses from addresses.test.txt
```

#### Action: Add address

```bash
# Add a single address
curl -s -X POST http://127.0.0.1:3000/v1/addresses \
  -H "Content-Type: application/json" \
  -d '{"address": "0x2546bcd268d7f0b1be2a7d3e3c5e2f8b1a4d6c9e"}' | jq .

# Add multiple addresses at once
curl -s -X POST http://127.0.0.1:3000/v1/addresses \
  -H "Content-Type: application/json" \
  -d '{"addresses": ["0xabcd1234abcd1234abcd1234abcd1234abcd1234", "0x1111222233334444555566667777888899990000"]}' | jq .
```

#### Verify: Addresses added

```bash
ADDRS=$(curl -s http://127.0.0.1:3000/v1/addresses)
ADDR_COUNT=$(echo "$ADDRS" | jq 'length')
[ "$ADDR_COUNT" -ge 5 ] && echo "PASS: addresses added ($ADDR_COUNT)" || echo "FAIL: address count ($ADDR_COUNT)"

# Verify the single-added address is present
echo "$ADDRS" | jq -e 'map(select(. == "0x2546bcd268d7f0b1be2a7d3e3c5e2f8b1a4d6c9e")) | length == 1' > /dev/null && echo "PASS: single address added" || echo "FAIL: single address missing"
```

#### Action: Remove address

```bash
# Remove the single-added address
curl -s -X DELETE http://127.0.0.1:3000/v1/addresses/0x2546bcd268d7f0b1be2a7d3e3c5e2f8b1a4d6c9e | jq .
```

#### Verify: Address removed

```bash
ADDRS=$(curl -s http://127.0.0.1:3000/v1/addresses)
echo "$ADDRS" | jq -e 'map(select(. == "0x2546bcd268d7f0b1be2a7d3e3c5e2f8b1a4d6c9e")) | length == 0' > /dev/null && echo "PASS: address removed" || echo "FAIL: address still present"

# Cleanup
kill $RP_PID 2>/dev/null
```

---

### 6.12 CLI — Add/Remove Chain via CLI

**Objective:** Verify that the `--add-chain` and `--remove-chain` CLI flags correctly modify `Config.toml` using `toml_edit` (comment-preserving TOML mutation).

#### Setup

```bash
# Make a backup of the config
cp Config.test.toml Config.test.toml.bak
```

#### Action: Add chain

```bash
# Add a new chain via CLI
cargo run -- \
  --config Config.test.toml \
  --addresses addresses.test.txt \
  --add-chain "avalanche,eip155:43114,https://api.avax.network/ext/bc/C/rpc,https://rpc.ankr.com/avalanche"
```

#### Verify: Chain added

```bash
# Load the config and verify
cargo run -- \
  --config Config.test.toml \
  --addresses addresses.test.txt \
  --verbose 2>&1 | rg "chains"

# Verify the chain is in the TOML
rg '\[chains.avalanche\]' Config.test.toml && echo "PASS: avalanche chain in TOML" || echo "FAIL: avalanche chain missing"
rg 'eip155:43114' Config.test.toml && echo "PASS: caip2 in TOML" || echo "FAIL: caip2 missing"

# Verify comments are preserved
rg "# --- EVM" Config.test.toml && echo "PASS: comments preserved after CLI add" || echo "FAIL: comments lost after CLI add"
```

#### Action: Remove chain

```bash
# Remove the avalanche chain via CLI
cargo run -- \
  --config Config.test.toml \
  --addresses addresses.test.txt \
  --remove-chain "avalanche"
```

#### Verify: Chain removed

```bash
# Verify the chain is gone
rg '\[chains.avalanche\]' Config.test.toml && echo "FAIL: avalanche still in TOML" || echo "PASS: avalanche removed from TOML"

# Verify original chains intact
rg '\[chains.anvil\]' Config.test.toml && echo "PASS: anvil intact" || echo "FAIL: anvil corrupted"
rg '\[chains.solana\]' Config.test.toml && echo "PASS: solana intact" || echo "FAIL: solana corrupted"
rg '\[chains.bitcoin\]' Config.test.toml && echo "PASS: bitcoin intact" || echo "FAIL: bitcoin corrupted"

# Restore backup
mv Config.test.toml.bak Config.test.toml
```

---

### 6.13 CLI — Add/Remove Asset via CLI

**Objective:** Verify that the `--add-asset` and `--remove-asset` CLI flags correctly add and remove assets nested under their parent chain using `toml_edit`.

#### Setup

```bash
cp Config.test.toml Config.test.toml.bak
```

#### Action: Add asset

```bash
# Add a new ERC-20 asset to the anvil chain
cargo run -- \
  --config Config.test.toml \
  --addresses addresses.test.txt \
  --add-asset "anvil,DAI,0x6B175474E89094C44Da98b954EedeAC495271d0F,18"
```

#### Verify: Asset added

```bash
# Verify the asset is nested under [chains.anvil.assets.DAI]
rg '\[chains.anvil.assets.DAI\]' Config.test.toml && echo "PASS: DAI asset in TOML" || echo "FAIL: DAI asset missing"
rg '0x6B175474E89094C44Da98b954EedeAC495271d0F' Config.test.toml && echo "PASS: DAI contract in TOML" || echo "FAIL: DAI contract missing"

# Verify comments are preserved
rg "# --- EVM" Config.test.toml && echo "PASS: comments preserved" || echo "FAIL: comments lost"

# Verify the asset has no caip2 field (v0.8.0)
# The TOML should have only contract and decimals under [chains.anvil.assets.DAI]
DAI_SECTION=$(rg -A2 '\[chains.anvil.assets.DAI\]' Config.test.toml)
echo "$DAI_SECTION" | rg "caip2" && echo "FAIL: asset has caip2 (should not)" || echo "PASS: asset has no caip2"
```

#### Action: Remove asset

```bash
# Remove the DAI asset from the anvil chain
cargo run -- \
  --config Config.test.toml \
  --addresses addresses.test.txt \
  --remove-asset "anvil,DAI"
```

#### Verify: Asset removed

```bash
rg '\[chains.anvil.assets.DAI\]' Config.test.toml && echo "FAIL: DAI still in TOML" || echo "PASS: DAI removed from TOML"

# Verify other assets under anvil are intact
rg '\[chains.anvil.assets.ETH_NATIVE\]' Config.test.toml && echo "PASS: ETH_NATIVE intact" || echo "FAIL: ETH_NATIVE corrupted"
rg '\[chains.anvil.assets.MTK\]' Config.test.toml && echo "PASS: MTK intact" || echo "FAIL: MTK corrupted"

# Restore backup
mv Config.test.toml.bak Config.test.toml
```

---

### 6.14 Graceful Shutdown (Ctrl+C)

**Objective:** Verify that `tokio::select!` with `ctrl_c()` allows the daemon to shut down gracefully, saving scan progress.

#### Setup

```bash
# Start rustplorer in daemon mode
cargo run -- \
  --config Config.test.toml \
  --addresses addresses.test.txt \
  --api-port 3000 \
  --watch \
  --interval 5 \
  --verbose \
  &> /tmp/rustplorer-shutdown.log &

RP_PID=$!
sleep 3
echo "rustplorer started (PID: $RP_PID)"
```

#### Action

```bash
# Send SIGINT (Ctrl+C) to the process
kill -INT $RP_PID

# Wait for the process to exit
sleep 3
```

#### Verify

```bash
# Check the log for graceful shutdown message
rg "Graceful shutdown" /tmp/rustplorer-shutdown.log && echo "PASS: graceful shutdown logged" || echo "FAIL: no shutdown message"

# Verify the process has exited (not running anymore)
if kill -0 $RP_PID 2>/dev/null; then
  echo "FAIL: process still running after SIGINT"
  kill -9 $RP_PID
else
  echo "PASS: process terminated cleanly"
fi
```

---

### 6.15 Multi-Chain Concurrent Scanning

**Objective:** Verify that rustplorer concurrently scans multiple chains and correctly aggregates deposits from all chains via the MPSC channel.

#### Setup

```bash
# Ensure all three local chains are running
# Ensure addresses for all chains are in addresses.test.txt
```

#### Action

```bash
# On EVM: send 1 ETH
cast send --rpc-url http://127.0.0.1:8545 \
  --private-key 0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80 \
  0x70997970c51812dc3a010c7d01b50e0d17dc79c8 --value 1ether

# On Bitcoin: send 0.5 BTC (assuming bitcoind is set up)
# TARGET_BTC_ADDR should already be in addresses.test.txt
# bitcoin-cli -regtest -rpcuser=rpcuser -rpcpassword=rpcpassword -rpcport=18443 sendtoaddress "$TARGET_BTC_ADDR" 0.5

# Record approximate block numbers
BLOCK_BEFORE=$(cast block-number --rpc-url http://127.0.0.1:8545)

sleep 2

BLOCK_AFTER=$(cast block-number --rpc-url http://127.0.0.1:8545)
```

#### Verify

```bash
OUTPUT=$(cargo run -- \
  --config Config.test.toml \
  --addresses addresses.test.txt \
  --start-block "$BLOCK_BEFORE" \
  --end-block "$BLOCK_AFTER" \
  --format json 2>/dev/null)

# Verify deposits from multiple chains are present
EVM_DEPOSITS=$(echo "$OUTPUT" | jq '[.[] | select(.chain == "anvil")] | length')
[ "$EVM_DEPOSITS" -gt 0 ] && echo "PASS: EVM deposits found ($EVM_DEPOSITS)" || echo "FAIL: no EVM deposits"

# Verify each deposit has the correct chain
echo "$OUTPUT" | jq -e '[.[] | select(.chain == "anvil")][0].chain == "anvil"' > /dev/null && echo "PASS: chain = anvil" || echo "FAIL: chain mismatch"

# If BTC deposits exist, verify chain
BTC_DEPOSITS=$(echo "$OUTPUT" | jq '[.[] | select(.chain == "bitcoin")] | length')
if [ "$BTC_DEPOSITS" -gt 0 ]; then
  echo "$OUTPUT" | jq -e '[.[] | select(.chain == "bitcoin")][0].chain == "bitcoin"' > /dev/null && echo "PASS: BTC chain" || echo "FAIL: BTC chain"
fi
```

---

### 6.16 Dashboard UI — Address Management

**Objective:** Verify that the web dashboard at `GET /` allows adding and removing tracked addresses.

> **Note:** This test requires Playwright or manual browser interaction. The steps below describe manual verification.

#### Setup

```bash
# Start rustplorer in daemon mode with API
cargo run -- \
  --config Config.test.toml \
  --addresses addresses.test.txt \
  --api-port 3000 \
  --watch \
  --interval 10 \
  &> /tmp/rustplorer-dashboard.log &

RP_PID=$!
sleep 3
```

#### Verify: Dashboard loads

```bash
# Verify the index.html is served
DASHBOARD=$(curl -s http://127.0.0.1:3000/)
echo "$DASHBOARD" | rg "Rustplorer Deposit Monitor" && echo "PASS: dashboard HTML served" || echo "FAIL: dashboard not served"
echo "$DASHBOARD" | rg "Tracked Addresses" && echo "PASS: sidebar present" || echo "FAIL: sidebar missing"
echo "$DASHBOARD" | rg "Recent Deposits" && echo "PASS: feed present" || echo "FAIL: feed missing"
```

#### Verify: Add Address modal

```bash
# Verify the Add Address button is in the HTML
echo "$DASHBOARD" | rg "addAddressBtn" && echo "PASS: add address button present" || echo "FAIL: add address button missing"

# Verify the modal form
echo "$DASHBOARD" | rg "addAddressModal" && echo "PASS: add address modal present" || echo "FAIL: add address modal missing"
echo "$DASHBOARD" | rg "addrValue" && echo "PASS: address input present" || echo "FAIL: address input missing"
```

#### Verify: Address list via API (used by dashboard JS)

```bash
# The dashboard JS fetches addresses from /v1/addresses
ADDRS=$(curl -s http://127.0.0.1:3000/v1/addresses)
echo "$ADDRS" | jq -e 'length > 0' > /dev/null && echo "PASS: addresses API returns data" || echo "FAIL: addresses API empty"

# Add an address and verify it appears
curl -s -X POST http://127.0.0.1:3000/v1/addresses \
  -H "Content-Type: application/json" \
  -d '{"address": "0xDDdDddDdDdDDDDDDDDDDDDDDdDDdddDDdDDDDDDDD"}'

ADDRS_AFTER=$(curl -s http://127.0.0.1:3000/v1/addresses)
echo "$ADDRS_AFTER" | jq -e 'map(select(. == "0xdddddddddddddddddddddddddddddddddddddddd")) | length == 1' > /dev/null && echo "PASS: new address in list" || echo "FAIL: new address not found"
```

#### Cleanup

```bash
kill $RP_PID 2>/dev/null
```

---

### 6.17 Dashboard UI — Settings Modal (Chains & Assets tabs)

**Objective:** Verify that the Settings modal in the dashboard shows Chains and Assets tabs, and that the add/remove operations work correctly through the API calls triggered by the UI.

#### Setup

```bash
# Start rustplorer in daemon mode with API
cargo run -- \
  --config Config.test.toml \
  --addresses addresses.test.txt \
  --api-port 3000 \
  --watch \
  --interval 10 \
  &> /tmp/rustplorer-settings.log &

RP_PID=$!
sleep 3
```

#### Verify: Settings modal HTML

```bash
DASHBOARD=$(curl -s http://127.0.0.1:3000/)

# Verify the settings button
echo "$DASHBOARD" | rg "settingsBtn" && echo "PASS: settings button present" || echo "FAIL: settings button missing"

# Verify the settings modal
echo "$DASHBOARD" | rg "settingsModal" && echo "PASS: settings modal present" || echo "FAIL: settings modal missing"

# Verify the Chains tab
echo "$DASHBOARD" | rg 'data-settings-tab="chains"' && echo "PASS: chains tab present" || echo "FAIL: chains tab missing"

# Verify the Assets tab
echo "$DASHBOARD" | rg 'data-settings-tab="assets"' && echo "PASS: assets tab present" || echo "FAIL: assets tab missing"

# Verify the Add Chain form fields
echo "$DASHBOARD" | rg "chainName" && echo "PASS: chain name input present" || echo "FAIL: chain name input missing"
echo "$DASHBOARD" | rg "chainCaip2" && echo "PASS: CAIP-2 input present" || echo "FAIL: CAIP-2 input missing"
echo "$DASHBOARD" | rg "chainRpcs" && echo "PASS: RPC URLs input present" || echo "FAIL: RPC URLs input missing"
echo "$DASHBOARD" | rg "addChainSubmit" && echo "PASS: add chain button present" || echo "FAIL: add chain button missing"

# Verify the Add Asset form fields
echo "$DASHBOARD" | rg "assetChainName" && echo "PASS: asset chain name input present" || echo "FAIL: asset chain name input missing"
echo "$DASHBOARD" | rg "assetContract" && echo "PASS: asset contract input present" || echo "FAIL: asset contract input missing"
echo "$DASHBOARD" | rg "assetDecimals" && echo "PASS: asset decimals input present" || echo "FAIL: asset decimals input missing"
echo "$DASHBOARD" | rg "addAssetSubmit" && echo "PASS: add asset button present" || echo "FAIL: add asset button missing"
```

#### Verify: Chains API (used by Chains tab JS)

```bash
# The settings modal JS fetches config from /v1/config
CONFIG=$(curl -s http://127.0.0.1:3000/v1/config)

# Verify the nested chain structure
echo "$CONFIG" | jq -e '.data.chains.anvil' > /dev/null && echo "PASS: anvil chain in config" || echo "FAIL: anvil chain missing"
echo "$CONFIG" | jq -e '.data.chains.anvil.assets.ETH_NATIVE' > /dev/null && echo "PASS: nested assets in config" || echo "FAIL: nested assets missing"

# Add a chain via the API (same call the dashboard makes)
curl -s -X POST http://127.0.0.1:3000/v1/chains \
  -H "Content-Type: application/json" \
  -d '{"name":"optimism","caip2":"eip155:10","rpc":["https://mainnet.optimism.io"]}'

# Verify it appears in the config
CONFIG=$(curl -s http://127.0.0.1:3000/v1/config)
echo "$CONFIG" | jq -e '.data.chains.optimism.caip2 == "eip155:10"' > /dev/null && echo "PASS: optimism added" || echo "FAIL: optimism not added"

# Remove it
curl -s -X DELETE http://127.0.0.1:3000/v1/chains/optimism
CONFIG=$(curl -s http://127.0.0.1:3000/v1/config)
echo "$CONFIG" | jq -e '.data.chains.optimism == null' > /dev/null && echo "PASS: optimism removed" || echo "FAIL: optimism still present"
```

#### Verify: Assets API (used by Assets tab JS)

```bash
# Add an asset via the API (same call the dashboard makes)
curl -s -X POST http://127.0.0.1:3000/v1/assets \
  -H "Content-Type: application/json" \
  -d '{"chain":"anvil","name":"WETH","contract":"0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2","decimals":18}'

# Verify it appears nested under the anvil chain
CONFIG=$(curl -s http://127.0.0.1:3000/v1/config)
echo "$CONFIG" | jq -e '.data.chains.anvil.assets.WETH.contract == "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"' > /dev/null && echo "PASS: WETH added under anvil" || echo "FAIL: WETH not added"

# Remove it
curl -s -X DELETE http://127.0.0.1:3000/v1/assets/anvil/WETH
CONFIG=$(curl -s http://127.0.0.1:3000/v1/config)
echo "$CONFIG" | jq -e '.data.chains.anvil.assets.WETH == null' > /dev/null && echo "PASS: WETH removed" || echo "FAIL: WETH still present"
```

#### Cleanup

```bash
kill $RP_PID 2>/dev/null
```

---

## 7. Automated E2E Test Script

The following bash script runs all E2E scenarios sequentially, reporting PASS/FAIL for each.

```bash
#!/usr/bin/env bash
set -euo pipefail

# ============================================================
# rustplorer v0.8.0 Automated E2E Test Suite
# ============================================================

PASS_COUNT=0
FAIL_COUNT=0
SKIP_COUNT=0

pass() { echo "  ✅ PASS: $1"; ((++PASS_COUNT)); }
fail() { echo "  ❌ FAIL: $1"; ((++FAIL_COUNT)); }
skip() { echo "  ⏭  SKIP: $1"; ((++SKIP_COUNT)); }

CONFIG="Config.test.toml"
ADDRS="addresses.test.txt"
API_PORT=3000
RP_PID=""

# ============================================================
# Phase 0: Build
# ============================================================
echo "══════════════════════════════════════════════════════"
echo "Phase 0: Building rustplorer..."
echo "══════════════════════════════════════════════════════"

cargo build --release 2>&1 || { echo "BUILD FAILED"; exit 1; }
echo "Build successful."

# ============================================================
# Phase 1: Unit Tests
# ============================================================
echo ""
echo "══════════════════════════════════════════════════════"
echo "Phase 1: Running unit tests..."
echo "══════════════════════════════════════════════════════"

if cargo test --all-targets 2>&1; then
  pass "Unit tests"
else
  fail "Unit tests"
fi

# ============================================================
# Phase 2: Check Local Chains
# ============================================================
echo ""
echo "══════════════════════════════════════════════════════"
echo "Phase 2: Checking local chain availability..."
echo "══════════════════════════════════════════════════════"

ANVIL_UP=false
SOLANA_UP=false
BTC_UP=false

if cast block-number --rpc-url http://127.0.0.1:8545 &>/dev/null; then
  ANVIL_UP=true
  pass "anvil is running"
else
  skip "anvil not running (EVM tests skipped)"
fi

if solana slot --url http://127.0.0.1:8899 &>/dev/null; then
  SOLANA_UP=true
  pass "solana-test-validator is running"
else
  skip "solana-test-validator not running (Solana tests skipped)"
fi

if bitcoin-cli -regtest -rpcuser=rpcuser -rpcpassword=rpcpassword -rpcport=18443 getblockcount &>/dev/null; then
  BTC_UP=true
  bitcoin-cli -regtest -rpcuser=rpcuser -rpcpassword=rpcpassword -rpcport=18443 createwallet "rustplorer_e2e" &>/dev/null || true
  pass "bitcoind is running"
else
  skip "bitcoind not running (Bitcoin tests skipped)"
fi

# ============================================================
# Helper: Start/Stop Daemon
# ============================================================
start_daemon() {
  cargo run --release -- \
    --config "$CONFIG" \
    --addresses "$ADDRS" \
    --api-port $API_PORT \
    --watch \
    --interval 5 \
    &> /tmp/rustplorer-e2e.log &
  RP_PID=$!
  sleep 3
}

stop_daemon() {
  if [ -n "$RP_PID" ] && kill -0 "$RP_PID" 2>/dev/null; then
    kill -INT "$RP_PID" 2>/dev/null || true
    sleep 2
    kill -9 "$RP_PID" 2>/dev/null || true
    RP_PID=""
  fi
}

# ============================================================
# Phase 3: E2E Scenarios
# ============================================================
echo ""
echo "══════════════════════════════════════════════════════"
echo "Phase 3: E2E Scenarios"
echo "══════════════════════════════════════════════════════"

SENDER_KEY="0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
TARGET_1="0x70997970c51812dc3a010c7d01b50e0d17dc79c8"
TARGET_2="0x3c44cdddb6a900fa2b585dd299e03d12fa4293bc"

# ---- 6.1 EVM Native ETH Deposit ----
echo ""
echo "--- 6.1: EVM Native ETH Deposit ---"
if $ANVIL_UP; then
  BLOCK_BEFORE=$(cast block-number --rpc-url http://127.0.0.1:8545)
  cast send --rpc-url http://127.0.0.1:8545 --private-key "$SENDER_KEY" "$TARGET_1" --value 1ether &>/dev/null
  BLOCK_AFTER=$(cast block-number --rpc-url http://127.0.0.1:8545)

  OUTPUT=$(cargo run --release -- \
    --config "$CONFIG" --addresses "$ADDRS" \
    --network eip155:31337 \
    --start-block "$BLOCK_BEFORE" --end-block "$BLOCK_AFTER" \
    --format json 2>/dev/null)

  echo "$OUTPUT" | jq -e '.[0].chain == "anvil"' > /dev/null && pass "6.1 chain" || fail "6.1 chain"
  echo "$OUTPUT" | jq -e '.[0].asset == "Native"' > /dev/null && pass "6.1 asset" || fail "6.1 asset"
  echo "$OUTPUT" | jq -e '.[0].amount_clean == "1"' > /dev/null && pass "6.1 amount" || fail "6.1 amount"
else
  skip "6.1 EVM Native ETH Deposit (anvil not running)"
fi

# ---- 6.2 EVM ERC-20 Token Deposit ----
echo ""
echo "--- 6.2: EVM ERC-20 Token Deposit ---"
if $ANVIL_UP && [ -n "${MOCK_TOKEN_ADDR:-}" ]; then
  BLOCK_BEFORE=$(cast block-number --rpc-url http://127.0.0.1:8545)
  cast send --rpc-url http://127.0.0.1:8545 --private-key "$SENDER_KEY" \
    "$MOCK_TOKEN_ADDR" "transfer(address,uint256)" "$TARGET_2" 50000000 &>/dev/null
  BLOCK_AFTER=$(cast block-number --rpc-url http://127.0.0.1:8545)

  OUTPUT=$(cargo run --release -- \
    --config "$CONFIG" --addresses "$ADDRS" \
    --network eip155:31337 \
    --start-block "$BLOCK_BEFORE" --end-block "$BLOCK_AFTER" \
    --format json 2>/dev/null)

  MTK_DEPOSIT=$(echo "$OUTPUT" | jq '[.[] | select(.asset == "MTK")] | .[0]')
  echo "$MTK_DEPOSIT" | jq -e '.asset == "MTK"' > /dev/null && pass "6.2 asset" || fail "6.2 asset"
  echo "$MTK_DEPOSIT" | jq -e '.chain == "anvil"' > /dev/null && pass "6.2 chain" || fail "6.2 chain"
  echo "$MTK_DEPOSIT" | jq -e '.amount_clean == "50"' > /dev/null && pass "6.2 amount" || fail "6.2 amount"
else
  skip "6.2 EVM ERC-20 Token Deposit (anvil or MOCK_TOKEN_ADDR not set)"
fi

# ---- 6.3 EVM Batched ERC-20 ----
echo ""
echo "--- 6.3: EVM Batched ERC-20 ---"
if $ANVIL_UP && [ -n "${MOCK_TOKEN_ADDR:-}" ] && [ -n "${MOCK_TOKEN_B_ADDR:-}" ]; then
  BLOCK_BEFORE=$(cast block-number --rpc-url http://127.0.0.1:8545)
  cast send --rpc-url http://127.0.0.1:8545 --private-key "$SENDER_KEY" \
    "$MOCK_TOKEN_ADDR" "transfer(address,uint256)" "$TARGET_2" 50000000 &>/dev/null
  cast send --rpc-url http://127.0.0.1:8545 --private-key "$SENDER_KEY" \
    "$MOCK_TOKEN_B_ADDR" "transfer(address,uint256)" "$TARGET_2" 25000000000000000000 &>/dev/null
  BLOCK_AFTER=$(cast block-number --rpc-url http://127.0.0.1:8545)

  OUTPUT=$(cargo run --release -- \
    --config "$CONFIG" --addresses "$ADDRS" \
    --network eip155:31337 \
    --start-block "$BLOCK_BEFORE" --end-block "$BLOCK_AFTER" \
    --format json 2>/dev/null)

  MTK_COUNT=$(echo "$OUTPUT" | jq '[.[] | select(.asset == "MTK")] | length')
  TUSD_COUNT=$(echo "$OUTPUT" | jq '[.[] | select(.asset == "TUSD")] | length')
  [ "$MTK_COUNT" -ge 1 ] && pass "6.3 MTK detected" || fail "6.3 MTK not detected"
  [ "$TUSD_COUNT" -ge 1 ] && pass "6.3 TUSD detected" || fail "6.3 TUSD not detected"
else
  skip "6.3 EVM Batched ERC-20 (tokens not deployed)"
fi

# ---- 6.4 EVM Auto End Block ----
echo ""
echo "--- 6.4: EVM Auto End Block Detection ---"
if $ANVIL_UP; then
  cat > /tmp/Config.auto_end.toml <<'EOF'
[chains.anvil]
caip2 = "eip155:31337"
rpc = ["http://127.0.0.1:8545"]
start_block = 0

  [chains.anvil.assets.ETH_NATIVE]
  contract = "native"
  decimals = 18
EOF

  OUTPUT=$(cargo run --release -- \
    --config /tmp/Config.auto_end.toml --addresses "$ADDRS" \
    --format json 2>/dev/null)

  DEPOSIT_COUNT=$(echo "$OUTPUT" | jq 'length')
  [ "$DEPOSIT_COUNT" -gt 0 ] && pass "6.4 auto end_block" || fail "6.4 auto end_block"
else
  skip "6.4 EVM Auto End Block (anvil not running)"
fi

# ---- 6.5 Solana Native SOL Deposit ----
echo ""
echo "--- 6.5: Solana Native SOL Deposit ---"
if $SOLANA_UP; then
  TARGET_KEY=$(solana-keygen new --no-bip39-passphrase --force --outfile /tmp/sol-e2e-target.json 2>/dev/null | grep "pubkey:" | awk '{print $2}')
  echo "$TARGET_KEY" >> "$ADDRS"

  solana airdrop 10 --url http://127.0.0.1:8899 &>/dev/null
  SLOT_BEFORE=$(solana slot --url http://127.0.0.1:8899)
  solana transfer --url http://127.0.0.1:8899 --allow-unfunded-recipient "$TARGET_KEY" 2.5 &>/dev/null
  sleep 2
  SLOT_AFTER=$(solana slot --url http://127.0.0.1:8899)

  OUTPUT=$(cargo run --release -- \
    --config "$CONFIG" --addresses "$ADDRS" \
    --network "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp" \
    --start-block "$SLOT_BEFORE" --end-block "$SLOT_AFTER" \
    --format json 2>/dev/null)

  SOL_DEPOSIT=$(echo "$OUTPUT" | jq '[.[] | select(.asset == "Native")] | .[0]')
  echo "$SOL_DEPOSIT" | jq -e '.chain == "solana"' > /dev/null && pass "6.5 chain" || fail "6.5 chain"
  echo "$SOL_DEPOSIT" | jq -e '.amount_clean == "2.5"' > /dev/null && pass "6.5 amount" || fail "6.5 amount"
else
  skip "6.5 Solana Native SOL Deposit (solana not running)"
fi

# ---- 6.6 Bitcoin Native BTC Deposit ----
echo ""
echo "--- 6.6: Bitcoin Native BTC Deposit ---"
if $BTC_UP; then
  TARGET_BTC=$(bitcoin-cli -regtest -rpcuser=rpcuser -rpcpassword=rpcpassword -rpcport=18443 getnewaddress "e2e")
  echo "$TARGET_BTC" >> "$ADDRS"
  MINER=$(bitcoin-cli -regtest -rpcuser=rpcuser -rpcpassword=rpcpassword -rpcport=18443 getnewaddress "miner")
  bitcoin-cli -regtest -rpcuser=rpcuser -rpcpassword=rpcpassword -rpcport=18443 generatetoaddress 101 "$MINER" &>/dev/null

  BLOCK_BEFORE=$(bitcoin-cli -regtest -rpcuser=rpcuser -rpcpassword=rpcpassword -rpcport=18443 getblockcount)
  bitcoin-cli -regtest -rpcuser=rpcuser -rpcpassword=rpcpassword -rpcport=18443 sendtoaddress "$TARGET_BTC" 1.5 &>/dev/null
  bitcoin-cli -regtest -rpcuser=rpcuser -rpcpassword=rpcpassword -rpcport=18443 generatetoaddress 1 "$MINER" &>/dev/null
  BLOCK_AFTER=$(bitcoin-cli -regtest -rpcuser=rpcuser -rpcpassword=rpcpassword -rpcport=18443 getblockcount)

  OUTPUT=$(cargo run --release -- \
    --config "$CONFIG" --addresses "$ADDRS" \
    --network "bip122:000000000019d6689c085ae165831e93" \
    --start-block "$BLOCK_BEFORE" --end-block "$BLOCK_AFTER" \
    --format json 2>/dev/null)

  BTC_DEPOSIT=$(echo "$OUTPUT" | jq '[.[] | select(.asset == "Native")] | .[0]')
  echo "$BTC_DEPOSIT" | jq -e '.chain == "bitcoin"' > /dev/null && pass "6.6 chain" || fail "6.6 chain"
  echo "$BTC_DEPOSIT" | jq -e '.amount_clean == "1.5"' > /dev/null && pass "6.6 amount" || fail "6.6 amount"
else
  skip "6.6 Bitcoin Native BTC Deposit (bitcoind not running)"
fi

# ---- 6.7 Bitcoin Precision Test ----
echo ""
echo "--- 6.7: Bitcoin Precision Test ---"
if $BTC_UP; then
  TARGET_PREC=$(bitcoin-cli -regtest -rpcuser=rpcuser -rpcpassword=rpcpassword -rpcport=18443 getnewaddress "prec")
  echo "$TARGET_PREC" >> "$ADDRS"

  BLOCK_BEFORE=$(bitcoin-cli -regtest -rpcuser=rpcuser -rpcpassword=rpcpassword -rpcport=18443 getblockcount)
  bitcoin-cli -regtest -rpcuser=rpcuser -rpcpassword=rpcpassword -rpcport=18443 sendtoaddress "$TARGET_PREC" 0.12345678 &>/dev/null
  bitcoin-cli -regtest -rpcuser=rpcuser -rpcpassword=rpcpassword -rpcport=18443 generatetoaddress 1 "$MINER" &>/dev/null
  BLOCK_AFTER=$(bitcoin-cli -regtest -rpcuser=rpcuser -rpcpassword=rpcpassword -rpcport=18443 getblockcount)

  OUTPUT=$(cargo run --release -- \
    --config "$CONFIG" --addresses "$ADDRS" \
    --network "bip122:000000000019d6689c085ae165831e93" \
    --start-block "$BLOCK_BEFORE" --end-block "$BLOCK_AFTER" \
    --format json 2>/dev/null)

  BTC_DEPOSIT=$(echo "$OUTPUT" | jq '[.[] | select(.to_address == "'$TARGET_PREC'")] | .[0]')
  echo "$BTC_DEPOSIT" | jq -e '.amount_raw == "12345678"' > /dev/null && pass "6.7 precision raw" || fail "6.7 precision raw"
  echo "$BTC_DEPOSIT" | jq -e '.amount_clean == "0.12345678"' > /dev/null && pass "6.7 precision clean" || fail "6.7 precision clean"
else
  skip "6.7 Bitcoin Precision Test (bitcoind not running)"
fi

# ---- 6.8-6.11: API Tests ----
echo ""
echo "--- 6.8-6.11: API Tests ---"
if $ANVIL_UP; then
  start_daemon

  # 6.8: Add/Remove Chain
  curl -s -X POST http://127.0.0.1:3000/v1/chains \
    -H "Content-Type: application/json" \
    -d '{"name":"testchain","caip2":"eip155:999","rpc":["http://127.0.0.1:8545"]}' &>/dev/null
  CONFIG_RESP=$(curl -s http://127.0.0.1:3000/v1/config)
  echo "$CONFIG_RESP" | jq -e '.data.chains.testchain.caip2 == "eip155:999"' > /dev/null && pass "6.8 add chain" || fail "6.8 add chain"

  curl -s -X DELETE http://127.0.0.1:3000/v1/chains/testchain &>/dev/null
  CONFIG_RESP=$(curl -s http://127.0.0.1:3000/v1/config)
  echo "$CONFIG_RESP" | jq -e '.data.chains.testchain == null' > /dev/null && pass "6.8 remove chain" || fail "6.8 remove chain"

  # 6.9: Add/Remove Asset
  curl -s -X POST http://127.0.0.1:3000/v1/assets \
    -H "Content-Type: application/json" \
    -d '{"chain":"anvil","name":"TESTTK","contract":"0x0000000000000000000000000000000000000001","decimals":18}' &>/dev/null
  CONFIG_RESP=$(curl -s http://127.0.0.1:3000/v1/config)
  echo "$CONFIG_RESP" | jq -e '.data.chains.anvil.assets.TESTTK.decimals == 18' > /dev/null && pass "6.9 add asset" || fail "6.9 add asset"
  echo "$CONFIG_RESP" | jq -e '.data.chains.anvil.assets.TESTTK | has("caip2") | not' > /dev/null && pass "6.9 no caip2 on asset" || fail "6.9 asset has caip2"

  curl -s -X DELETE http://127.0.0.1:3000/v1/assets/anvil/TESTTK &>/dev/null
  CONFIG_RESP=$(curl -s http://127.0.0.1:3000/v1/config)
  echo "$CONFIG_RESP" | jq -e '.data.chains.anvil.assets.TESTTK == null' > /dev/null && pass "6.9 remove asset" || fail "6.9 remove asset"

  # 6.10: Ring Buffer /v1/deposits
  START_MS=$(python3 - <<'PY'
import time
print(int(time.time() * 1000))
PY
)
  DEPOSITS=$(curl -s http://127.0.0.1:3000/v1/deposits)
  END_MS=$(python3 - <<'PY'
import time
print(int(time.time() * 1000))
PY
)
  ELAPSED_MS=$((END_MS - START_MS))
  [ "$ELAPSED_MS" -lt 500 ] && pass "6.10 fast response (${ELAPSED_MS}ms)" || fail "6.10 slow response (${ELAPSED_MS}ms)"

  # 6.11: Address Management
  curl -s -X POST http://127.0.0.1:3000/v1/addresses \
    -H "Content-Type: application/json" \
    -d '{"address":"0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}' &>/dev/null
  ADDRS_RESP=$(curl -s http://127.0.0.1:3000/v1/addresses)
  echo "$ADDRS_RESP" | jq -e '.data | map(select(. == "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")) | length == 1' > /dev/null && pass "6.11 add address" || fail "6.11 add address"

  curl -s -X DELETE http://127.0.0.1:3000/v1/addresses/0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa &>/dev/null
  ADDRS_RESP=$(curl -s http://127.0.0.1:3000/v1/addresses)
  echo "$ADDRS_RESP" | jq -e '.data | map(select(. == "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")) | length == 0' > /dev/null && pass "6.11 remove address" || fail "6.11 remove address"

  stop_daemon
else
  skip "6.8-6.11 API Tests (anvil not running)"
fi

# ---- 6.12-6.13: CLI Tests ----
echo ""
echo "--- 6.12-6.13: CLI Chain/Asset Management ---"
cp "$CONFIG" "${CONFIG}.bak"

# 6.12: CLI Add/Remove Chain
cargo run --release -- --config "$CONFIG" --addresses "$ADDRS" \
  --add-chain "testchain,eip155:999,http://127.0.0.1:8545" &>/dev/null
rg '\[chains.testchain\]' "$CONFIG" &>/dev/null && pass "6.12 add chain CLI" || fail "6.12 add chain CLI"

cargo run --release -- --config "$CONFIG" --addresses "$ADDRS" \
  --remove-chain "testchain" &>/dev/null
rg '\[chains.testchain\]' "$CONFIG" &>/dev/null && fail "6.12 remove chain CLI" || pass "6.12 remove chain CLI"

# 6.13: CLI Add/Remove Asset
cargo run --release -- --config "$CONFIG" --addresses "$ADDRS" \
  --add-asset "anvil,TESTTK,0x0000000000000000000000000000000000000001,18" &>/dev/null
rg '\[chains.anvil.assets.TESTTK\]' "$CONFIG" &>/dev/null && pass "6.13 add asset CLI" || fail "6.13 add asset CLI"

cargo run --release -- --config "$CONFIG" --addresses "$ADDRS" \
  --remove-asset "anvil,TESTTK" &>/dev/null
rg '\[chains.anvil.assets.TESTTK\]' "$CONFIG" &>/dev/null && fail "6.13 remove asset CLI" || pass "6.13 remove asset CLI"

# Restore config
mv "${CONFIG}.bak" "$CONFIG"

# ---- 6.14: Graceful Shutdown ----
echo ""
echo "--- 6.14: Graceful Shutdown ---"
if $ANVIL_UP; then
  start_daemon
  kill -INT "$RP_PID" 2>/dev/null
  sleep 3
  if kill -0 "$RP_PID" 2>/dev/null; then
    fail "6.14 process still running after SIGINT"
    kill -9 "$RP_PID" 2>/dev/null || true
  else
    pass "6.14 process terminated cleanly"
  fi
  RP_PID=""
else
  skip "6.14 Graceful Shutdown (anvil not running)"
fi

# ---- 6.16-6.17: Dashboard UI ----
echo ""
echo "--- 6.16-6.17: Dashboard UI ---"
if $ANVIL_UP; then
  start_daemon
  DASHBOARD=$(curl -s http://127.0.0.1:3000/)
  echo "$DASHBOARD" | rg "Rustplorer" &>/dev/null && pass "6.16 dashboard HTML" || fail "6.16 dashboard HTML"
  echo "$DASHBOARD" | rg 'id="addChainBtn"' &>/dev/null && pass "6.17 chains management" || fail "6.17 chains management"
  echo "$DASHBOARD" | rg 'id="addAssetBtn"' &>/dev/null && pass "6.17 assets management" || fail "6.17 assets management"
  stop_daemon
else
  skip "6.16-6.17 Dashboard UI (anvil not running)"
fi

# ============================================================
# Summary
# ============================================================
echo ""
echo "══════════════════════════════════════════════════════"
echo "E2E Test Summary"
echo "══════════════════════════════════════════════════════"
echo "  ✅ PASS:  $PASS_COUNT"
echo "  ❌ FAIL:  $FAIL_COUNT"
echo "  ⏭  SKIP:  $SKIP_COUNT"
echo "══════════════════════════════════════════════════════"

if [ "$FAIL_COUNT" -gt 0 ]; then
  exit 1
fi
```

---

## 8. Docker Compose Setup

For a reproducible test environment, use Docker Compose to spin up all local chains:

```yaml
# docker-compose.test.yml
version: "3.9"

services:
  # --- EVM Local Node ---
  anvil:
    image: ghcr.io/foundry-rs/foundry:latest
    command: ["anvil", "--host", "0.0.0.0", "--port", "8545", "--chain-id", "31337", "--block-time", "1"]
    ports:
      - "8545:8545"
      - "8546:8546"
    healthcheck:
      test: ["CMD", "curl", "-s", "-X", "POST", "http://localhost:8545", "-H", "Content-Type: application/json", "-d", '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}']
      interval: 5s
      timeout: 3s
      retries: 10

  # --- Solana Local Validator ---
  solana-test-validator:
    image: ghcr.io/solana-labs/solana:latest
    command: ["solana-test-validator", "--rpc-port", "8899", "--quiet"]
    ports:
      - "8899:8899"
      - "8900:8900"
    healthcheck:
      test: ["CMD", "curl", "-s", "http://localhost:8899", "-X", "POST", "-H", "Content-Type: application/json", "-d", '{"jsonrpc":"2.0","method":"getSlot","params":[],"id":1}']
      interval: 10s
      timeout: 5s
      retries: 20

  # --- Bitcoin Core (regtest) ---
  bitcoind:
    image: lncm/bitcoind:v24.0
    command: ["bitcoind", "-regtest", "-server=1", "-rpcuser=rpcuser", "-rpcpassword=rpcpassword", "-rpcport=18443", "-rpcallowip=0.0.0.0/0", "-rpcbind=0.0.0.0", "-fallbackfee=0.0001"]
    ports:
      - "18443:18443"
    healthcheck:
      test: ["CMD", "bitcoin-cli", "-regtest", "-rpcuser=rpcuser", "-rpcpassword=rpcpassword", "-rpcport=18443", "getblockcount"]
      interval: 5s
      timeout: 3s
      retries: 10

  # --- rustplorer daemon ---
  rustplorer:
    build:
      context: .
      dockerfile: Dockerfile
    command: [
      "rustplorer",
      "--config", "/app/Config.test.toml",
      "--addresses", "/app/addresses.test.txt",
      "--api-port", "3000",
      "--watch",
      "--interval", "10",
      "--verbose"
    ]
    ports:
      - "3000:3000"
    volumes:
      - ./Config.test.toml:/app/Config.test.toml:ro
      - ./addresses.test.txt:/app/addresses.test.txt:ro
    depends_on:
      anvil:
        condition: service_healthy
      bitcoind:
        condition: service_healthy
    # Note: Solana health check may be slow; rustplorer handles RPC failures gracefully
```

### 8.1 Running the Docker Compose Stack

```bash
# Start all services
docker compose -f docker-compose.test.yml up -d

# Wait for all services to be healthy
docker compose -f docker-compose.test.yml ps

# View rustplorer logs
docker compose -f docker-compose.test.yml logs -f rustplorer

# Run tests against the Docker stack
# (The API is available at http://127.0.0.1:3000)
curl -s http://127.0.0.1:3000/v1/config | jq '.chains | keys'

# Tear down
docker compose -f docker-compose.test.yml down -v
```

### 8.2 Deploying MockToken on Docker anvil

```bash
# Deploy using the Foundry container
MOCK_DIR=$(mktemp -d /tmp/mock-token-XXXXXX)
cd "$MOCK_DIR"

# Copy MockToken.sol into a new forge project
# ... (see Section 3.1.4 for the contract source)

forge create \
  --rpc-url http://127.0.0.1:8545 \
  --private-key 0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80 \
  src/MockToken.sol:MockToken \
  --constructor-args "Mock Token" "MTK" 6 1000000000000 \
  --json | jq -r '.deployedTo'
```

---

## 9. Known Limitations

| # | Limitation | Impact | Mitigation |
|---|-----------|--------|------------|
| 1 | **Solana `getBlock` requires full transaction details** | High memory usage per slot; may OOM on high-throughput mainnet slots | Use `rpc_options.max_concurrent = 1` and `rpc_options.delay_ms = 500` for Solana mainnet |
| 2 | **Bitcoin Core verbosity 3 required** | `getblock` with verbosity 3 includes `prevout` data for `from_address` extraction | No alternative; ensure `bitcoind` v24+ is used |
| 3 | **Ring buffer cap 100** | Deposits older than the last 100 are not available via `/v1/deposits` API | Check the JSONL output file for full history, or increase the cap in `main.rs` |
| 4 | **API binds to 127.0.0.1 only** | No remote access to the API by default | Override with `--host` flag; be aware of security implications |
| 5 | **No TLS/HTTPS on API** | API traffic is unencrypted | Run behind a reverse proxy (nginx/caddy) in production |
| 6 | **toml_edit round-trip formatting** | Minor whitespace changes may occur during TOML mutation | Comments and key order are preserved; values are accurate |
| 7 | **EVM address matching is case-insensitive** | Addresses are stored in lowercase; checksummed forms are normalized | This is by design — `alloy-primitives` validates then lowercases |
| 8 | **Bitcoin `from_address` is best-effort** | Coinjoin and complex scripts may not yield a meaningful `from_address` | The first `vin.prevout` address is used; may show "unknown" |
| 9 | **No WebSocket support** | All chain communication is via HTTP JSON-RPC polling | Future versions may add WebSocket for real-time updates |
| 10 | **Batched `eth_getLogs` may hit node limits** | Some RPC providers limit the number of addresses in a single `eth_getLogs` call | Use `rpc_options` to control concurrency; split into multiple calls if needed |
| 11 | **Config hot-reload is per-cycle** | Adding a chain/asset via API takes effect on the next watch cycle | Set `--interval` to a low value (e.g., 5) for faster testing |
| 12 | **No asset `caip2` field (v0.8.0)** | Old v0.7.0 configs with `[assets.X].caip2` will fail to parse | Migrate to v0.8.0 nested format (`[chains.NAME.assets.X]`) |

---

## 10. Troubleshooting

### 10.1 "Failed to parse block hex"

**Symptom:** `Failed to parse block hex ''` in logs.

**Cause:** The RPC endpoint returned an empty or non-hex result for `eth_blockNumber`.

**Fix:**
1. Verify the RPC URL is correct and accessible: `curl -X POST http://127.0.0.1:8545 -H "Content-Type: application/json" -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}'`
2. Check that `anvil` is running: `cast block-number --rpc-url http://127.0.0.1:8545`
3. Try a different RPC endpoint from the `rpc` array.

### 10.2 "All N RPC endpoints exhausted after 3 rounds"

**Symptom:** Scanner fails to fetch any data from all RPC endpoints.

**Cause:** All endpoints are down, rate-limited, or returning errors.

**Fix:**
1. Check each RPC URL individually with `curl`.
2. For public RPCs, check rate limits — add `rpc_options.delay_ms` to slow down.
3. Add more RPC URLs to the `rpc` array for fallback.

### 10.3 "No [chains] section found in config"

**Symptom:** API or CLI returns "No [chains] section found".

**Cause:** The `Config.toml` file is empty or uses the old v0.7.0 `[[chains]]` format.

**Fix:** Convert to the v0.8.0 nested format:
```toml
# OLD (v0.7.0):
[[chains]]
caip2 = "eip155:1"

# NEW (v0.8.0):
[chains.ethereum]
caip2 = "eip155:1"
```

### 10.4 "Asset has caip2 field" / Config parse error

**Symptom:** `toml_edit::de::from_str` fails with unknown field `caip2` on `AssetConfig`.

**Cause:** v0.8.0 removed the `caip2` field from `AssetConfig`. Assets inherit their chain's CAIP-2 identifier.

**Fix:** Remove `caip2` from all `[chains.NAME.assets.X]` sections.

### 10.5 BTC precision loss (wrong satoshi count)

**Symptom:** `amount_raw` shows `12345677` instead of `12345678`.

**Cause:** The `serde_json` crate was compiled without the `arbitrary_precision` feature, causing `f64` parsing of BTC values.

**Fix:** Ensure `Cargo.toml` has:
```toml
serde_json = { version = "1", features = ["arbitrary_precision"] }
```

### 10.6 Comments lost after API/CLI mutation

**Symptom:** Comments in `Config.toml` are removed after adding/removing a chain or asset.

**Cause:** The `toml_edit` library preserves comments by operating on `DocumentMut` instead of re-serializing from Rust types.

**Fix:** This should not happen in v0.8.0. If it does, file a bug — the `manage_chains_cli` and `manage_assets_cli` functions use `DocumentMut` for all mutations.

### 10.7 API not responding

**Symptom:** `curl http://127.0.0.1:3000/v1/config` times out.

**Cause:** The API is not started (missing `--api-port` flag) or the port is in use.

**Fix:**
1. Ensure `--api-port 3000` is passed when starting the daemon.
2. Check if the port is in use: `lsof -i :3000`
3. Use a different port: `--api-port 3001`

### 10.8 Solana "slot skipped" warnings

**Symptom:** `Failed to fetch slot` warnings in logs for certain slot numbers.

**Cause:** The Solana test validator may skip slots or the `getBlock` RPC returns null for empty or skipped slots.

**Fix:** This is expected behavior. The scanner logs a warning and continues to the next slot.

### 10.9 "Graceful shutdown" not working

**Symptom:** The daemon does not shut down after Ctrl+C.

**Cause:** The `tokio::select!` branch may be blocked on a long-running RPC call.

**Fix:**
1. Send a second Ctrl+C (SIGINT) after 5 seconds.
2. If still stuck, use `kill -9` as a last resort.

### 10.10 Ring buffer empty on `/v1/deposits`

**Symptom:** `GET /v1/deposits` returns `{"data":[],"meta":{"total":0}}` even though deposits have occurred.

**Cause:** The ring buffer is only updated after a scan cycle completes in watch mode.

**Fix:**
1. Wait for a full scan cycle (default: 60 seconds, use `--interval 5` for testing).
2. Ensure the deposit occurred within the scanned block range.
3. Check the JSONL output file for deposit records (the ring buffer is a subset).

### 10.11 EVM address not detected

**Symptom:** A known deposit is not picked up by rustplorer.

**Cause:** EVM addresses in `addresses.txt` are normalized to lowercase by `alloy-primitives`. If the `to_address` from the RPC doesn't match the lowercase form, the deposit is missed.

**Fix:**
1. Ensure addresses in `addresses.txt` are valid EVM addresses (starting with `0x`).
2. Invalid addresses are skipped with a `tracing::warn!` — check the logs.
3. The scanner compares lowercase forms, so mixed-case addresses in the file are fine.

### 10.12 Docker: bitcoind RPC not accessible from rustplorer container

**Symptom:** rustplorer in Docker cannot connect to bitcoind's RPC.

**Cause:** The bitcoind RPC binds to localhost by default, which doesn't include Docker's network.

**Fix:** Add `-rpcallowip=0.0.0.0/0 -rpcbind=0.0.0.0` to the bitcoind command (already included in the Docker Compose config above). In the `Config.test.toml`, use the Docker service name:
```toml
[chains.bitcoin]
rpc = ["http://rpcuser:rpcpassword@bitcoind:18443"]
```
