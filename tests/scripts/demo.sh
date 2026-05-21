#!/usr/bin/env bash
# =============================================================================
# rustplorer — Live Dashboard Demo Script
#
# Starts local blockchain nodes (anvil + solana-test-validator + bitcoin regtest) if available,
# launches the rustplorer daemon, then runs a background transaction generator
# for native EVM/Solana/Bitcoin, ERC-20, fee-on-transfer ERC-20, and SPL token
# deposits so the built-in dashboard at http://localhost:DEMO_PORT/ shows live updates.
#
# The dashboard shows live deposit updates every INTERVAL seconds.
#
# Safely copies Config.example.toml and addresses.example.txt to a temp
# directory — never mutates checked-in files. No secrets are required
# beyond the well-known test defaults (anvil dev keys, etc.).
#
# Cleanup: all local processes and temp files are removed on exit (Ctrl+C).
#
# Usage:
#   chmod +x tests/scripts/demo.sh
#   ./tests/scripts/demo.sh [--port PORT] [--interval SECONDS]
#
# Flags:
#   --port PORT        API port (default: 3000)
#   --interval SECONDS Watch polling interval (default: 15)
#   --open              Open the dashboard in your browser (macOS only)
#   --no-local-chains   Skip starting local chains; use public RPCs only
#
# Environment:
#   RUSTPLORER_BIN=/path/to/rustplorer  Use this binary instead of cargo run.
# =============================================================================
set -euo pipefail

DEMO_PORT="3000"
INTERVAL="15"
OPEN_BROWSER="false"
NO_LOCAL_CHAINS="false"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --port)       DEMO_PORT="$2"; shift 2 ;;
        --interval)   INTERVAL="$2";  shift 2 ;;
        --open)       OPEN_BROWSER="true"; shift ;;
        --no-local-chains) NO_LOCAL_CHAINS="true"; shift ;;
        -h|--help)
            echo "Usage: $0 [--port PORT] [--interval SECONDS] [--open] [--no-local-chains]"
            echo ""
            echo "  --port PORT          API port (default: 3000)"
            echo "  --interval SECONDS   Watch polling interval (default: 15)"
            echo "  --open               Open dashboard in browser (macOS)"
            echo "  --no-local-chains    Skip local chains; use public RPCs only"
            exit 0
            ;;
        *) echo "Unknown flag: $1"; exit 1 ;;
    esac
done

PROJECT_DIR="$(cd "$(dirname "$0")/../.." && pwd)"
DEMO_DIR="$(mktemp -d /tmp/rustplorer-demo-XXXXXX)"

ANVIL_PID=""
SOLANA_PID=""
DAEMON_PID=""
GENERATOR_PID=""
BTC_CONTAINER="rustplorer-bitcoin-demo"
_DID_CLEANUP=false

# Well-known local dev addresses / keys (anvil #1, anvil #2, solana random)
ANVIL_TARGET="0x70997970c51812dc3a010c7d01b50e0d17dc79c8"
SOL_TARGET="3zCGKxMK3JHNUMtHbticPoDvoRbUgzY65ayoHMWZwZE2"
SENDER_KEY="0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
BTC_TARGET=""
BTC_RPCUSER="rpcuser"
BTC_RPCPASS="rpcpassword"
BTC_PORT="18444"
SOLANA_FAUCET_PORT=$((9900 + (RANDOM % 1000)))
ANVIL_START_BLOCK="0"
SOLANA_START_SLOT="0"
BTC_START_BLOCK="0"

cleanup() {
    # Idempotent: guard so the EXIT trap re-fire after 'exit 0' is a no-op.
    if $_DID_CLEANUP; then
        return 0
    fi
    _DID_CLEANUP=true

    echo ""
    echo "Cleaning up demo environment..."

    # ---- Stop transaction generator ----
    if [ -n "${GENERATOR_PID:-}" ]; then
        echo "  Stopping transaction generator (PID: $GENERATOR_PID)..."
        # Kill children first so the subshell can't spawn more work
        pkill -P "$GENERATOR_PID" 2>/dev/null || true
        kill "$GENERATOR_PID" 2>/dev/null || true
        sleep 1
        # Force-kill stragglers
        kill -0 "$GENERATOR_PID" 2>/dev/null && kill -9 "$GENERATOR_PID" 2>/dev/null || true
        pkill -9 -P "$GENERATOR_PID" 2>/dev/null || true
        wait "$GENERATOR_PID" 2>/dev/null || true
        GENERATOR_PID=""
    fi

    # ---- Stop daemon ----
    if [ -n "${DAEMON_PID:-}" ]; then
        echo "  Stopping daemon (PID: $DAEMON_PID)..."
        kill -INT "$DAEMON_PID" 2>/dev/null || true
        sleep 2
        kill -0 "$DAEMON_PID" 2>/dev/null && kill -9 "$DAEMON_PID" 2>/dev/null || true
        wait "$DAEMON_PID" 2>/dev/null || true
        DAEMON_PID=""
    fi

    # ---- Stop anvil ----
    if [ -n "${ANVIL_PID:-}" ]; then
        echo "  Stopping anvil (PID: $ANVIL_PID)..."
        kill "$ANVIL_PID" 2>/dev/null || true
        sleep 1
        kill -0 "$ANVIL_PID" 2>/dev/null && kill -9 "$ANVIL_PID" 2>/dev/null || true
        wait "$ANVIL_PID" 2>/dev/null || true
        ANVIL_PID=""
    fi

    docker rm -f "$BTC_CONTAINER" 2>/dev/null || true

    # ---- Stop solana-test-validator ----
    if [ -n "${SOLANA_PID:-}" ]; then
        echo "  Stopping solana-test-validator (PID: $SOLANA_PID)..."
        pkill -P "$SOLANA_PID" 2>/dev/null || true
        kill "$SOLANA_PID" 2>/dev/null || true
        sleep 1
        kill -0 "$SOLANA_PID" 2>/dev/null && kill -9 "$SOLANA_PID" 2>/dev/null || true
        pkill -9 -P "$SOLANA_PID" 2>/dev/null || true
        wait "$SOLANA_PID" 2>/dev/null || true
        SOLANA_PID=""
    fi

    rm -rf "$DEMO_DIR"
    echo "Demo cleanup complete."
}

# Separate traps: EXIT for normal termination; INT/TERM explicitly exit
# after cleanup so the shell does not linger.
trap cleanup EXIT
trap 'cleanup; exit 0' INT TERM

echo "══════════════════════════════════════════════════════"
echo "  rustplorer — Live Dashboard Demo"
echo "══════════════════════════════════════════════════════"
echo ""

# ---- Check prerequisites ----
need_cmd() { command -v "$1" &>/dev/null || true; }

abort_if_missing() {
    if ! command -v "$1" &>/dev/null; then
        echo "ERROR: '$1' is required. $2"
        exit 1
    fi
}

abort_if_missing curl "Install curl"
abort_if_missing jq   "Install jq: brew install jq"

# ---- Prepare config and addresses from examples ----
echo "Preparing configuration..."
cp "$PROJECT_DIR/Config.example.toml" "$DEMO_DIR/Config.toml"
cp "$PROJECT_DIR/addresses.example.txt" "$DEMO_DIR/addresses.txt"

CONFIG="$DEMO_DIR/Config.toml"
ADDRS="$DEMO_DIR/addresses.txt"

# ---- Build rustplorer unless a binary was supplied ----
cd "$PROJECT_DIR"
if [ -n "${RUSTPLORER_BIN:-}" ]; then
    echo "Using rustplorer binary: $RUSTPLORER_BIN"
else
    echo "Building rustplorer..."
    cargo build -q 2>&1 || {
        echo "ERROR: Build failed. Make sure you have Rust 1.95.0+ installed."
        exit 1
    }
    RUSTPLORER_BIN="$PROJECT_DIR/target/debug/rustplorer"
    echo "  rustplorer built successfully."
fi

# ---- Optional: Start local chains ----
if $NO_LOCAL_CHAINS; then
    echo ""
    echo "Skipping local chains (--no-local-chains). Using public RPCs from Config.example.toml."
else
    echo ""
    echo "Attempting to start local chains for live transactions..."
    echo "------------------------------------------------------"

    # --- Start anvil ---
    if need_cmd anvil; then
        echo "Starting anvil (local EVM)..."
        anvil --host 127.0.0.1 --port 8545 --silent &>/tmp/anvil-demo.log &
        ANVIL_PID=$!
        sleep 2
        if cast block-number --rpc-url http://127.0.0.1:8545 &>/dev/null; then
            echo "  anvil started on :8545"
        else
            echo "  WARNING: anvil may not have started correctly — stopping it"
            kill "$ANVIL_PID" 2>/dev/null || true
            wait "$ANVIL_PID" 2>/dev/null || true
            ANVIL_PID=""
        fi
    else
        echo "  SKIP: anvil not found (install Foundry: curl -L https://foundry.paradigm.xyz | bash)"
    fi

    # --- Deploy MockToken (ERC-20 for richer demo) ---
    ANVIL_RPC="http://127.0.0.1:8545"

    if [ -n "$ANVIL_PID" ] && need_cmd forge && need_cmd cast; then
        WORKDIR=$(mktemp -d)
        mkdir -p "$WORKDIR/src"
        cat > "$WORKDIR/src/MockToken.sol" << 'SOL'
// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;
contract MockToken {
    string public name = "MockToken";
    string public symbol = "MTK";
    uint8 public decimals = 6;
    uint256 public totalSupply;
    mapping(address => uint256) public balanceOf;
    event Transfer(address indexed from, address indexed to, uint256 value);
    constructor() { totalSupply = 1000000 * 10**6; balanceOf[msg.sender] = totalSupply; emit Transfer(address(0), msg.sender, totalSupply); }
    function transfer(address to, uint256 value) public returns (bool) { require(balanceOf[msg.sender] >= value, "insufficient"); balanceOf[msg.sender] -= value; balanceOf[to] += value; emit Transfer(msg.sender, to, value); return true; }
}

contract FeeToken {
    string public name = "FeeToken";
    string public symbol = "FTK";
    uint8 public decimals = 6;
    uint256 public totalSupply;
    mapping(address => uint256) public balanceOf;
    event Transfer(address indexed from, address indexed to, uint256 value);
    constructor() { totalSupply = 1000000 * 10**6; balanceOf[msg.sender] = totalSupply; emit Transfer(address(0), msg.sender, totalSupply); }
    function transfer(address to, uint256 value) public returns (bool) {
        uint256 tax = value / 10;
        uint256 netValue = value - tax;
        require(balanceOf[msg.sender] >= value, "insufficient");
        balanceOf[msg.sender] -= value;
        balanceOf[to] += netValue;
        balanceOf[address(0xdead)] += tax;
        emit Transfer(msg.sender, to, netValue);
        return true;
    }
}
SOL
        cat > "$WORKDIR/foundry.toml" << 'TOML'
[profile.default]
src = "src"
out = "out"
libs = ["lib"]
TOML

        forge build --root "$WORKDIR" &>/dev/null
        TOKEN_ADDR=$(forge create --rpc-url "$ANVIL_RPC" --private-key "$SENDER_KEY" \
            --root "$WORKDIR" --broadcast src/MockToken.sol:MockToken 2>&1 | \
            grep "Deployed to:" | awk '{print $3}')
        FEE_TOKEN_ADDR=$(forge create --rpc-url "$ANVIL_RPC" --private-key "$SENDER_KEY" \
            --root "$WORKDIR" --broadcast src/MockToken.sol:FeeToken 2>&1 | \
            grep "Deployed to:" | awk '{print $3}')
        echo "  MockToken deployed at: $TOKEN_ADDR"
        echo "  FeeToken deployed at: $FEE_TOKEN_ADDR"

        rm -rf "$WORKDIR"

        # Create config: anvil with native ETH + ERC-20.
        # Start at the next block so the daemon's first scan catches the
        # deterministic seed deposits sent below instead of only future BTC
        # confirmations that happen after startup.
        ANVIL_TIP=$(cast block-number --rpc-url "$ANVIL_RPC" 2>/dev/null || echo 0)
        ANVIL_START_BLOCK=$((ANVIL_TIP + 1))
        cat > "$CONFIG" << TOML
[chains.anvil]
caip2 = "eip155:31337"
start_block = $ANVIL_START_BLOCK
rpc = ["http://127.0.0.1:8545"]

  [chains.anvil.assets.ETH_NATIVE]
  contract = "native"
  decimals = 18

  [chains.anvil.assets.MTK]
  contract = "$TOKEN_ADDR"
  decimals = 6

  [chains.anvil.assets.FTK]
  contract = "$FEE_TOKEN_ADDR"
  decimals = 6
TOML
    elif [ -n "$ANVIL_PID" ]; then
        # anvil running but no forge/cast — basic ETH-only config
        ANVIL_START_BLOCK="0"
        if need_cmd cast; then
            ANVIL_TIP=$(cast block-number --rpc-url "$ANVIL_RPC" 2>/dev/null || echo 0)
            ANVIL_START_BLOCK=$((ANVIL_TIP + 1))
        fi
        cat > "$CONFIG" << TOML
[chains.anvil]
caip2 = "eip155:31337"
start_block = $ANVIL_START_BLOCK
rpc = ["http://127.0.0.1:8545"]

  [chains.anvil.assets.ETH_NATIVE]
  contract = "native"
  decimals = 18
TOML
    fi

    # --- Start solana-test-validator ---
    if need_cmd solana-test-validator && need_cmd solana; then
        echo ""
        echo "Starting solana-test-validator..."
        solana-test-validator --reset --quiet --rpc-port 8899 --faucet-port "$SOLANA_FAUCET_PORT" &>/tmp/solana-demo.log &
        SOLANA_PID=$!

        SOLANA_HEALTHY="false"
        for i in $(seq 1 30); do
            if curl -s -X POST "http://localhost:8899" \
                -H "Content-Type: application/json" \
                -d '{"jsonrpc":"2.0","id":1,"method":"getHealth"}' | grep -q "ok" 2>/dev/null; then
                echo "  solana-test-validator ready after ${i}s"
                SOLANA_HEALTHY="true"
                break
            fi
            sleep 1
        done

        if [ "$SOLANA_HEALTHY" != "true" ]; then
            echo "  WARNING: solana-test-validator health check failed — stopping it"
            pkill -P "$SOLANA_PID" 2>/dev/null || true
            kill "$SOLANA_PID" 2>/dev/null || true
            wait "$SOLANA_PID" 2>/dev/null || true
            SOLANA_PID=""
        else
            # Set up funded payer keypair for the transaction generator
            SOLANA_PAYER="/tmp/rustplorer-demo-payer.json"
            if need_cmd solana-keygen; then
                solana-keygen new -o "$SOLANA_PAYER" --no-bip39-passphrase --force --silent 2>/dev/null || true
                export SOLANA_CONFIG="$DEMO_DIR/solana-cli.yml"
                solana config set --url http://localhost:8899 --keypair "$SOLANA_PAYER" &>/dev/null || true
                solana airdrop 100 -k "$SOLANA_PAYER" --url http://localhost:8899 &>/dev/null 2>&1 || true
                echo "  Solana payer funded for demo transfers"
            fi

            # Append Solana to config. Use the current slot as the lower bound;
            # local validators can lag a little, and the scanner filters by the
            # actual signature slot.
            SOLANA_START_SLOT=$(curl -s -X POST "http://localhost:8899" \
                -H "Content-Type: application/json" \
                -d '{"jsonrpc":"2.0","id":1,"method":"getSlot"}' | jq -r '.result // 0' 2>/dev/null || echo 0)
            cat >> "$CONFIG" << TOML

[chains.solana]
caip2 = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp"
start_block = $SOLANA_START_SLOT
rpc = ["http://127.0.0.1:8899"]

  [chains.solana.rpc_options]
  max_concurrent = 1
  delay_ms = 200

  [chains.solana.assets.SOL_NATIVE]
  contract = "native"
  decimals = 9
TOML
            if need_cmd spl-token; then
                SPL_MINT=$(spl-token create-token --decimals 9 --url http://localhost:8899 2>/dev/null | awk '/Creating token/ {print $3}')
                if [ -n "${SPL_MINT:-}" ]; then
                    spl-token create-account "$SPL_MINT" --url http://localhost:8899 &>/dev/null || true
                    spl-token mint "$SPL_MINT" 100000 --url http://localhost:8899 &>/dev/null || true
                    echo "  SPL demo mint created: $SPL_MINT"
                    cat >> "$CONFIG" << TOML

  [chains.solana.assets.SPL_MOCK]
  contract = "$SPL_MINT"
  decimals = 9
TOML
                fi
            else
                echo "  SKIP: spl-token not found (SPL demo disabled)"
            fi
        fi
    else
        echo "  SKIP: solana-test-validator not found"
    fi

    # --- Start Bitcoin regtest in Docker ---
    if need_cmd docker; then
        echo ""
        echo "Starting Bitcoin regtest (Docker)..."
        docker rm -f "$BTC_CONTAINER" &>/dev/null || true
        docker run -d --name "$BTC_CONTAINER" \
            -p "$BTC_PORT":"$BTC_PORT" \
            lncm/bitcoind:v24.0 \
            -regtest=1 -rpcuser="$BTC_RPCUSER" -rpcpassword="$BTC_RPCPASS" \
            -rpcport="$BTC_PORT" -rpcbind=0.0.0.0 -rpcallowip=0.0.0.0/0 \
            -server=1 -fallbackfee=0.00001 &>/tmp/bitcoin-demo.log || true
        BTC_UP="false"
        for i in $(seq 1 20); do
            if docker exec "$BTC_CONTAINER" bitcoin-cli -regtest -rpcuser="$BTC_RPCUSER" -rpcpassword="$BTC_RPCPASS" -rpcport="$BTC_PORT" getblockchaininfo &>/dev/null; then
                BTC_UP="true"; break
            fi
            sleep 1
        done
        if [ "$BTC_UP" = "true" ]; then
            _btc() { docker exec "$BTC_CONTAINER" bitcoin-cli -regtest -rpcuser="$BTC_RPCUSER" -rpcpassword="$BTC_RPCPASS" -rpcport="$BTC_PORT" "$@"; }
            _btc createwallet "demo" &>/dev/null || true
            _btc generatetoaddress 101 "$(_btc getnewaddress miner bech32)" &>/dev/null
            BTC_TARGET=$(_btc getnewaddress "rustplorer-demo" "bech32")
            BTC_TIP=$(_btc getblockcount 2>/dev/null || echo 0)
            BTC_START_BLOCK=$((BTC_TIP + 1))
            echo "  Bitcoin regtest ready; target: $BTC_TARGET"
            cat >> "$CONFIG" << TOML

[chains.bitcoin]
caip2 = "bip122:000000000019d6689c085ae165831e93"
start_block = $BTC_START_BLOCK
rpc = ["http://$BTC_RPCUSER:$BTC_RPCPASS@127.0.0.1:$BTC_PORT"]

  [chains.bitcoin.assets.BTC_NATIVE]
  contract = "native"
  decimals = 8
TOML
        else
            echo "  SKIP: bitcoind container failed to start"
        fi
    fi
fi

# ---- Build addresses file with tracked target addresses ----
if $NO_LOCAL_CHAINS; then
    # Preserve addresses.example.txt as-is for public-chain mode
    echo "Using addresses from addresses.example.txt for public chains."
else
    > "$ADDRS"  # truncate
    if [ -n "${ANVIL_PID:-}" ]; then
        echo "$ANVIL_TARGET" >> "$ADDRS"
    fi
    if [ -n "${SOLANA_PID:-}" ]; then
        echo "$SOL_TARGET" >> "$ADDRS"
    fi
    if [ -n "${BTC_TARGET:-}" ]; then
        echo "$BTC_TARGET" >> "$ADDRS"
    fi
    if [ ! -s "$ADDRS" ]; then
        # fallback: at least one known address
        echo "0x70997970c51812dc3a010c7d01b50e0d17dc79c8" > "$ADDRS"
    fi
fi

# ---- Seed deterministic deposits before daemon startup ----
# The dashboard/API ring buffer only contains deposits observed while the daemon
# is running. Sending one deposit per configured asset here, after start_block is
# written and before the first scan, makes the demo immediately show all enabled
# local chains/assets (ETH, ERC-20s, SOL, SPL, BTC) instead of depending on the
# timing of the background generator.
if ! $NO_LOCAL_CHAINS; then
    echo ""
    echo "Seeding initial local deposits for the dashboard..."

    if [ -n "${ANVIL_PID:-}" ] && kill -0 "$ANVIL_PID" 2>/dev/null && need_cmd cast; then
        if cast send --rpc-url http://127.0.0.1:8545 \
            --private-key "$SENDER_KEY" "$ANVIL_TARGET" \
            --value 0.01ether &>/dev/null 2>&1; then
            echo "  Seeded 0.01 ETH -> $ANVIL_TARGET"
        fi
        if [ -n "${TOKEN_ADDR:-}" ] && cast send "$TOKEN_ADDR" "transfer(address,uint256)" "$ANVIL_TARGET" 2500000 \
            --rpc-url http://127.0.0.1:8545 --private-key "$SENDER_KEY" &>/dev/null 2>&1; then
            echo "  Seeded 2.5 MTK -> $ANVIL_TARGET"
        fi
        if [ -n "${FEE_TOKEN_ADDR:-}" ] && cast send "$FEE_TOKEN_ADDR" "transfer(address,uint256)" "$ANVIL_TARGET" 10000000 \
            --rpc-url http://127.0.0.1:8545 --private-key "$SENDER_KEY" &>/dev/null 2>&1; then
            echo "  Seeded 9 FTK net -> $ANVIL_TARGET"
        fi
    fi

    if [ -n "${SOLANA_PID:-}" ] && kill -0 "$SOLANA_PID" 2>/dev/null && need_cmd solana; then
        PAYER_FLAG=""
        if [ -n "${SOLANA_PAYER:-}" ] && [ -f "$SOLANA_PAYER" ]; then
            PAYER_FLAG="-k $SOLANA_PAYER"
        fi
        # shellcheck disable=SC2086
        if solana transfer --url http://localhost:8899 \
            --allow-unfunded-recipient $PAYER_FLAG "$SOL_TARGET" 0.1 \
            &>/dev/null 2>&1; then
            echo "  Seeded 0.1 SOL -> $SOL_TARGET"
        fi
        if [ -n "${SPL_MINT:-}" ] && need_cmd spl-token && spl-token transfer "$SPL_MINT" 1.25 "$SOL_TARGET" --url http://localhost:8899 --fund-recipient &>/dev/null 2>&1; then
            echo "  Seeded 1.25 SPL_MOCK -> $SOL_TARGET"
        fi
    fi

    if [ -n "${BTC_TARGET:-}" ] && docker ps --format '{{.Names}}' | grep -q "^$BTC_CONTAINER$"; then
        if docker exec "$BTC_CONTAINER" bitcoin-cli -regtest -rpcuser="$BTC_RPCUSER" -rpcpassword="$BTC_RPCPASS" -rpcport="$BTC_PORT" sendtoaddress "$BTC_TARGET" 0.001 &>/dev/null 2>&1; then
            docker exec "$BTC_CONTAINER" bitcoin-cli -regtest -rpcuser="$BTC_RPCUSER" -rpcpassword="$BTC_RPCPASS" -rpcport="$BTC_PORT" generatetoaddress 1 "$(docker exec "$BTC_CONTAINER" bitcoin-cli -regtest -rpcuser="$BTC_RPCUSER" -rpcpassword="$BTC_RPCPASS" -rpcport="$BTC_PORT" getnewaddress miner bech32)" &>/dev/null 2>&1 || true
            echo "  Seeded 0.001 BTC -> $BTC_TARGET"
        fi
    fi
fi

# ---- Start the daemon ----
echo ""
echo "══════════════════════════════════════════════════════"
echo "Starting rustplorer daemon..."
echo "══════════════════════════════════════════════════════"

DAEMON_LOG="$DEMO_DIR/daemon.log"

RUST_LOG=info "$RUSTPLORER_BIN" \
    --config "$CONFIG" \
    --addresses "$ADDRS" \
    --api-port "$DEMO_PORT" \
    --watch \
    --interval "$INTERVAL" \
    --verbose \
    &> "$DAEMON_LOG" &

DAEMON_PID=$!

# Wait for the API to become available
echo "Waiting for API server to start..."
for i in $(seq 1 15); do
    if curl -s -o /dev/null -w "%{http_code}" "http://127.0.0.1:$DEMO_PORT/" 2>/dev/null | grep -q "200"; then
        echo "API ready after ${i}s"
        break
    fi
    sleep 1
done

# ---- Start background transaction generator ----
# Sends new deposits every INTERVAL seconds so the dashboard shows live updates.
if [ -n "${ANVIL_PID:-}" ] || [ -n "${SOLANA_PID:-}" ] || [ -n "${BTC_TARGET:-}" ]; then
    echo ""
    echo "Starting background transaction generator (interval: ${INTERVAL}s)..."
    (
        COUNT=0
        while true; do
            if [ -n "${ANVIL_PID:-}" ] && kill -0 "$ANVIL_PID" 2>/dev/null; then
                if cast send --rpc-url http://127.0.0.1:8545 \
                    --private-key "$SENDER_KEY" "$ANVIL_TARGET" \
                    --value 0.01ether &>/dev/null 2>&1; then
                    ((++COUNT))
                    echo "  [gen #${COUNT}] Sent 0.01 ETH -> $ANVIL_TARGET"
                fi
                if [ -n "${TOKEN_ADDR:-}" ] && cast send "$TOKEN_ADDR" "transfer(address,uint256)" "$ANVIL_TARGET" 2500000 \
                    --rpc-url http://127.0.0.1:8545 --private-key "$SENDER_KEY" &>/dev/null 2>&1; then
                    ((++COUNT))
                    echo "  [gen #${COUNT}] Sent 2.5 MTK -> $ANVIL_TARGET"
                fi
                if [ -n "${FEE_TOKEN_ADDR:-}" ] && cast send "$FEE_TOKEN_ADDR" "transfer(address,uint256)" "$ANVIL_TARGET" 10000000 \
                    --rpc-url http://127.0.0.1:8545 --private-key "$SENDER_KEY" &>/dev/null 2>&1; then
                    ((++COUNT))
                    echo "  [gen #${COUNT}] Sent 9 FTK net -> $ANVIL_TARGET"
                fi
            fi
            if [ -n "${SOLANA_PID:-}" ] && kill -0 "$SOLANA_PID" 2>/dev/null; then
                PAYER_FLAG=""
                if [ -n "${SOLANA_PAYER:-}" ] && [ -f "$SOLANA_PAYER" ]; then
                    PAYER_FLAG="-k $SOLANA_PAYER"
                fi
                # shellcheck disable=SC2086
                if solana transfer --url http://localhost:8899 \
                    --allow-unfunded-recipient $PAYER_FLAG "$SOL_TARGET" 0.1 \
                    &>/dev/null 2>&1; then
                    ((++COUNT))
                    echo "  [gen #${COUNT}] Sent 0.1 SOL -> $SOL_TARGET"
                fi
                if [ -n "${SPL_MINT:-}" ] && spl-token transfer "$SPL_MINT" 1.25 "$SOL_TARGET" --url http://localhost:8899 --fund-recipient &>/dev/null 2>&1; then
                    ((++COUNT))
                    echo "  [gen #${COUNT}] Sent 1.25 SPL_MOCK -> $SOL_TARGET"
                fi
            fi
            if [ -n "${BTC_TARGET:-}" ] && docker ps --format '{{.Names}}' | grep -q "^$BTC_CONTAINER$"; then
                if docker exec "$BTC_CONTAINER" bitcoin-cli -regtest -rpcuser="$BTC_RPCUSER" -rpcpassword="$BTC_RPCPASS" -rpcport="$BTC_PORT" sendtoaddress "$BTC_TARGET" 0.001 &>/dev/null 2>&1; then
                    docker exec "$BTC_CONTAINER" bitcoin-cli -regtest -rpcuser="$BTC_RPCUSER" -rpcpassword="$BTC_RPCPASS" -rpcport="$BTC_PORT" generatetoaddress 1 "$(docker exec "$BTC_CONTAINER" bitcoin-cli -regtest -rpcuser="$BTC_RPCUSER" -rpcpassword="$BTC_RPCPASS" -rpcport="$BTC_PORT" getnewaddress miner bech32)" &>/dev/null 2>&1 || true
                    ((++COUNT))
                    echo "  [gen #${COUNT}] Sent 0.001 BTC -> $BTC_TARGET"
                fi
            fi
            sleep "$INTERVAL"
        done
    ) &
    GENERATOR_PID=$!
    echo "  Generator PID: $GENERATOR_PID (sends deposits every ${INTERVAL}s)"
fi

echo ""
echo "══════════════════════════════════════════════════════"
echo "  Dashboard is LIVE"
echo ""
echo "  Open:  http://localhost:$DEMO_PORT/"
echo "  Logs:  $DAEMON_LOG"
echo ""
echo "  The dashboard will refresh with new deposits every"
echo "  $INTERVAL seconds. Addresses being watched:"
echo ""
grep -v '^$' "$ADDRS" | sed 's/^/    /'
echo ""
echo "  Press Ctrl+C to stop the demo and clean up."
echo "══════════════════════════════════════════════════════"
echo ""

# ---- Open browser (macOS) ----
if $OPEN_BROWSER && [[ "$OSTYPE" == "darwin"* ]]; then
    sleep 1
    open "http://localhost:$DEMO_PORT/"
fi

# ---- Wait for Ctrl+C ----
# Use a signal-friendly loop instead of 'wait' so INT/TERM traps fire
# promptly when the parent script receives a signal.
echo "Daemon PID: $DAEMON_PID (waiting for Ctrl+C...)"
while kill -0 "$DAEMON_PID" 2>/dev/null; do
    sleep 1
done
