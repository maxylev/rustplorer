---
name: rustplorer
description: Use when the user needs to set up, configure, run, or troubleshoot rustplorer — a high-performance multi-chain deposit detector for EVM, Solana, and Bitcoin. Trigger on mentions of rustplorer, monitoring blockchain deposits, tracking crypto addresses for incoming payments, multi-chain deposit detection, watching addresses across chains, or setting up a self-hosted deposit tracker. Also trigger when users ask about configuring TOML chain configs, RPC failover for blockchains, daemon/watch mode for blockchain monitoring, or the HTTP API for dynamic address management.
---

# rustplorer

Help users set up, configure, run, and troubleshoot [rustplorer](https://github.com/maxylev/rustplorer) — a high-performance multi-chain deposit detector that monitors EVM, Solana, and Bitcoin addresses using only public RPC endpoints.

## Core concepts

rustplorer works on a **push** model: instead of asking "did address X receive a deposit?" for each address, it downloads blocks and asks "does this block contain any of my tracked addresses?" All filtering happens locally in memory using a `HashSet` with O(1) lookup — this is how it scales to 1M+ addresses.

It scans three chain families:
- **EVM** (Ethereum, Base, Polygon, BSC, Arbitrum, etc.) — ERC-20/ERC-721 via `eth_getLogs`, native tokens via `eth_getBlockByNumber`
- **Solana** — Native SOL and SPL tokens via `getBlock`
- **Bitcoin** — Native BTC via `getblockhash` + `getblock` (verbosity 3, requires Bitcoin Core v24.0.0+)

Chains are identified by [CAIP-2](https://github.com/ChainAgnostic/CAIPs/blob/master/CAIPs/caip-2.md) IDs:
- EVM: `eip155:<chain_id>` (e.g., `eip155:1` for Ethereum, `eip155:8453` for Base)
- Solana: `solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp` (mainnet genesis hash)
- Bitcoin: `bip122:000000000019d6689c085ae165831e93` (mainnet genesis hash)

## Installation

rustplorer can be installed three ways. Recommend based on the user's setup:

**Docker (simplest)** — pre-built image at `ghcr.io/maxylev/rustplorer:latest`:
```bash
docker pull ghcr.io/maxylev/rustplorer:latest
```

**Cargo (if Rust is installed)**:
```bash
cargo install rustplorer
```

**From source**:
```bash
git clone https://github.com/maxylev/rustplorer.git
cd rustplorer
cargo install --path .
```

## Configuration

rustplorer uses a TOML config file (`Config.toml` by default, override with `-c`).

### Chain configuration (`[[chains]]`)

Each chain entry has:
- `caip2` (required) — CAIP-2 chain ID
- `rpc` (required) — Array of RPC URLs; order matters for failover
- `start_block` (optional) — First block to scan; auto-detected from node if omitted
- `end_block` (optional) — Last block to scan; auto-detected from node if omitted

**Block range auto-detection behavior:**

| `start_block` | `end_block` | Behavior |
|---|---|---|
| set | set | Scan `start_block` → `end_block` |
| set | omitted | Scan `start_block` → node tip |
| omitted | set | Scan `(end_block - lookback)` → `end_block` |
| omitted | omitted | Scan `(node tip - lookback)` → `node tip` |

Default lookback values:
- EVM: 1,000 blocks (~20 min on Ethereum, ~3 min on Polygon)
- Solana: 500 slots (~3-4 min)
- Bitcoin: 6 blocks (~1 hour)

**Important**: Always include at least 2 RPC endpoints per chain for failover. If an endpoint returns a 429 (rate limit), 5xx, or JSON-RPC error, rustplorer automatically tries the next one.

### Asset configuration (`[assets.NAME]`)

Each asset entry has:
- `network` (required) — Must match a chain's `caip2`
- `contract` (required) — Token contract address, or `"native"` for the gas token
- `decimals` (required) — Token decimal places (ETH=18, MATIC=18, SOL=9, BTC=8, USDC=6)

The asset name (e.g., `USDC_ETH`) is used in the `token` field of deposit output.

### Address file

One address per line. Supports mixed EVM, Solana, and Bitcoin in the same file:
```
0x71C7656EC7ab88b098defB751B7401B5f6d8976F
0x8Ba1f109551bD432803012645Ac136ddd64DBA72
AMYmXa54xZuS7rjeSX7E4YwNVKpNbhFHK9gP7jLCN3A
bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq
```

EVM addresses are case-insensitive (stored lowercase). Solana and Bitcoin addresses are stored as-is.

## Running

### Single scan

```bash
rustplorer --addresses addresses.txt                    # JSON to stdout
rustplorer -a addresses.txt -o deposits.json            # Save to file
rustplorer -a addresses.txt --format csv -o out.csv     # CSV output
rustplorer -a addresses.txt --verbose -o results.json   # With progress output
```

### CLI overrides

Override config behavior from the command line:
```bash
# Scan only one network
rustplorer -a addresses.txt --network eip155:1

# Override block range for that network
rustplorer -a addresses.txt --network eip155:137 --start-block 55000000 --end-block 55001000

# Override RPC endpoints (comma-separated)
rustplorer -a addresses.txt --network eip155:1 --rpc "https://rpc.ankr.com/eth,https://eth.llamarpc.com"
```

### Daemon mode (`--watch`)

Continuous polling that picks up where the last scan left off:
```bash
rustplorer -a addresses.txt --watch --interval 30
rustplorer -a addresses.txt --watch --interval 60 -o deposits.jsonl
```

Daemon behavior:
1. Runs a full scan cycle
2. Records the highest block scanned per chain
3. Sleeps for `--interval` seconds (default 60)
4. **Hot-reloads** the address file from disk — add/remove/edit addresses and changes take effect without restart
5. Starts next scan at `last_block + 1` (no missed blocks, no overlaps)

In watch mode, JSON output uses JSON Lines format (`.jsonl`) — one object per line, appended on each cycle. CSV uses standard CSV with headers on first write.

### HTTP API (`--api-port`)

Start a REST API for dynamic address management:
```bash
rustplorer -a addresses.txt --watch --interval 30 --api-port 3000
```

Endpoints:

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/addresses` | List all tracked addresses |
| `POST` | `/addresses` | Add address(es) |
| `DELETE` | `/addresses` | Remove address(es) |

POST/DELETE body accepts either `{"address": "0x..."}` for a single address or `{"addresses": ["0x...", "..."]}` for batches:
```bash
# Add one address
curl -X POST http://localhost:3000/addresses \
  -H "Content-Type: application/json" \
  -d '{"address": "0x71C7656EC7ab88b098defB751B7401B5f6d8976F"}'

# Add multiple
curl -X POST http://localhost:3000/addresses \
  -H "Content-Type: application/json" \
  -d '{"addresses": ["0xAAA...", "0xBBB..."]}'
```

### CLI address management

Edit the address file directly from the CLI (operates on the file and exits immediately):
```bash
rustplorer -a addresses.txt --add-address "0xNewAddress..."
rustplorer -a addresses.txt --add-address "0xAAA..." --add-address "0xBBB..."
rustplorer -a addresses.txt --remove-address "0xAAA..."
```

## Output format

### JSON output fields

Each deposit result has:
- `chain` — CAIP-2 chain ID where the deposit was detected
- `token` — Asset name from config (e.g., `USDC_ETH`, `ETH_NATIVE`, or `Native` for unnamed)
- `from_address` — Sender address
- `to_address` — Receiver address (matched from the address file)
- `amount_raw` — Raw hex (EVM), lamports (Solana), or satoshis (BTC)
- `amount_clean` — Human-readable decimal string using the configured decimals
- `block_number` — Block number (or slot for Solana) where the deposit occurred

### CSV output

Same fields as columns: `chain,token,from_address,to_address,amount_raw,amount_clean,block_number`

## Troubleshooting

### Common RPC issues

**"429 Too Many Requests" / rate limiting**: Add more RPC endpoints to the `rpc` array. Free public endpoints typically allow 5-10 req/sec. rustplorer's chunking strategy helps (200-block chunks for EVM `eth_getLogs`), but native ETH scanning requires 1 RPC call per block. For production, consider dedicated nodes.

**Solana "getBlock" failures**: Free Solana RPC endpoints often limit to ~100 requests per 10 seconds. If scanning more than ~100 slots, expect delays or failures. Use a paid RPC provider for production Solana scanning.

**Bitcoin requires verbosity 3**: The `getblock` verbosity 3 format (which includes `prevout` data) is supported by Bitcoin Core v24.0.0+. Not all public endpoints support this. If `publicnode.com` fails, try alternative Bitcoin RPC providers.

**EVM "execution reverted" or "max range exceeded"**: The `eth_getLogs` range is typically limited to 500-2,000 blocks. rustplorer chunks at 200 blocks — if you still see range errors, the RPC provider may have stricter limits. Try a different endpoint or reduce the block range.

### Block range issues

**"start_block > end_block, skipping"**: This message means the configured range is inverted. When using `--watch`, this can happen if the last scanned block is higher than the node's current tip (e.g., after a chain reorganization). It's harmless — the next cycle will resolve it.

**No deposits found when expected**: Verify:
1. The address is in the file and spelled correctly (EVM addresses are case-insensitive)
2. The chain config covers the block range where the deposit occurred
3. The asset config matches the token's network and contract address
4. RPC endpoints are responding (run with `--verbose` to see progress)

### Daemon-specific issues

**Address file changes not being picked up**: The hot-reload only happens at the start of each polling cycle. If you need changes reflected immediately, use the HTTP API instead (`--api-port`).

**Output file growing unboundedly**: In watch mode, results are appended. Consider log rotation or periodic cleanup for long-running daemons.

### Docker-specific

**File mounting**: When using Docker, config and address files must be mounted into the container. The internal default is `Config.toml` in the working directory (`/app/`), so mount and reference consistently:
```bash
docker run --rm \
  -v $(pwd)/Config.toml:/app/Config.toml \
  -v $(pwd)/addresses.txt:/app/addresses.txt \
  ghcr.io/maxylev/rustplorer:latest \
  -c /app/Config.toml -a /app/addresses.txt
```

## Example configurations

### Minimal: Ethereum only, native + USDC

```toml
[[chains]]
caip2 = "eip155:1"
rpc = ["https://eth.llamarpc.com", "https://rpc.ankr.com/eth"]

[assets.ETH]
network = "eip155:1"
contract = "native"
decimals = 18

[assets.USDC]
network = "eip155:1"
contract = "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"
decimals = 6
```

### Multi-chain with Solana and Bitcoin

```toml
[[chains]]
caip2 = "eip155:1"
rpc = ["https://eth.llamarpc.com", "https://cloudflare-eth.com"]

[[chains]]
caip2 = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp"
rpc = ["https://api.mainnet-beta.solana.com"]

[[chains]]
caip2 = "bip122:000000000019d6689c085ae165831e93"
rpc = ["https://bitcoin-rpc.publicnode.com"]

[assets.ETH]
network = "eip155:1"
contract = "native"
decimals = 18

[assets.SOL]
network = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp"
contract = "native"
decimals = 9

[assets.BTC]
network = "bip122:000000000019d6689c085ae165831e93"
contract = "native"
decimals = 8
```

### Production daemon with API

```bash
rustplorer -a addresses.txt --watch --interval 60 --api-port 3000 -o deposits.jsonl --verbose
```

## Reference

The full README is at `https://github.com/maxylev/rustplorer`. Key details are captured above — if the user needs something not covered here (e.g., programmatic Rust library usage, E2E testing with local chains), read the README directly.
