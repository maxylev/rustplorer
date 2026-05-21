#!/usr/bin/env bash
# =============================================================================
# rustplorer — full end-to-end test suite
#
# Covers: anvil (EVM), solana-test-validator (local), bitcoind (regtest docker),
#         ERC-20 deploy + transfers, SPL deploy + transfers, native transfers
#         on all chains, deposit detection (JSON/CSV), daemon mode, all REST API
#         endpoints, CLI management flags, graceful shutdown, Docker build + run.
#
# Prerequisites: docker, anvil, cast, forge, solana-test-validator, jq, curl
#
# Usage:
#   chmod +x tests/scripts/test_e2e_full.sh
#   ./tests/scripts/test_e2e_full.sh
# =============================================================================
set -euo pipefail

PASS=0; FAIL=0; SKIP=0
_pass() { echo "  ✅ PASS: $1"; ((++PASS)); }
_fail() { echo "  ❌ FAIL: $1"; ((++FAIL)); }
_skip() { echo "  ⏭  SKIP: $1"; ((++SKIP)); }

need_cmd() { command -v "$1" &>/dev/null || true; }
abort_if_missing() {
    if ! command -v "$1" &>/dev/null; then
        echo "❌ Required: $1 ($2)"
        exit 1
    fi
}

abort_if_missing docker  "Install Docker: https://docs.docker.com/get-docker/"
abort_if_missing anvil   "Install Foundry: curl -L https://foundry.paradigm.xyz | bash && foundryup"
abort_if_missing cast    "Install Foundry"
abort_if_missing forge   "Install Foundry"
abort_if_missing jq      "Install jq: brew install jq"
abort_if_missing curl    "Install curl"
abort_if_missing rg      "Install ripgrep: brew install ripgrep"

SOLANA_VALIDATOR_AVAILABLE=false
if need_cmd solana-test-validator && need_cmd solana && need_cmd solana-keygen; then
    SOLANA_VALIDATOR_AVAILABLE=true
fi
SOLANA_SPL_AVAILABLE=false
if $SOLANA_VALIDATOR_AVAILABLE && need_cmd spl-token; then
    SOLANA_SPL_AVAILABLE=true
fi

TESTDIR="/tmp/rustplorer-e2e-test"
rm -rf "$TESTDIR" && mkdir -p "$TESTDIR"

CONFIG="$TESTDIR/Config.toml"
ADDRS="$TESTDIR/addresses.txt"
API_PORT="3300"
LOG="$TESTDIR/daemon.log"
SOLANA_FAUCET_PORT=$((9900 + (RANDOM % 1000)))
BTC_HOST="127.0.0.1"
BTC_PORT="18443"
BTC_RPCUSER="rpcuser"
BTC_RPCPASS="rpcpassword"

PROJECT_DIR="$(cd "$(dirname "$0")/../.." && pwd)"
BTC_CONTAINER=""   # initialised early so cleanup trap can reference safely under set -u

# ============================================================
# Cleanup trap — must be set early so failures clean up
# ============================================================
_DID_CHAIN_CLEANUP=false
cleanup_chains() {
    # Guard: avoid double-run when INT/TERM handler calls 'exit 0'
    # which re-fires the EXIT trap.
    if $_DID_CHAIN_CLEANUP; then
        return 0
    fi
    _DID_CHAIN_CLEANUP=true

    docker rm -f "$BTC_CONTAINER" 2>/dev/null || true
    if [ -n "${ANVIL_PID:-}" ]; then
        kill "$ANVIL_PID" 2>/dev/null || true
        sleep 1
        kill -0 "$ANVIL_PID" 2>/dev/null && kill -9 "$ANVIL_PID" 2>/dev/null || true
        wait "$ANVIL_PID" 2>/dev/null || true
        ANVIL_PID=""
    fi
    if [ -n "${SOLANA_PID:-}" ]; then
        pkill -P "$SOLANA_PID" 2>/dev/null || true
        kill "$SOLANA_PID" 2>/dev/null || true
        sleep 1
        kill -0 "$SOLANA_PID" 2>/dev/null && kill -9 "$SOLANA_PID" 2>/dev/null || true
        pkill -9 -P "$SOLANA_PID" 2>/dev/null || true
        wait "$SOLANA_PID" 2>/dev/null || true
        SOLANA_PID=""
    fi
    if [ -n "${DAEMON_PID:-}" ] && kill -0 "$DAEMON_PID" 2>/dev/null; then
        kill -INT "$DAEMON_PID" 2>/dev/null || true
        sleep 2
        kill -0 "$DAEMON_PID" 2>/dev/null && kill -9 "$DAEMON_PID" 2>/dev/null || true
        wait "$DAEMON_PID" 2>/dev/null || true
        DAEMON_PID=""
    fi
}
trap cleanup_chains EXIT
trap 'cleanup_chains; exit 0' INT TERM

# ============================================================
# Phase 1: Start Local Chains
# ============================================================
echo ""
echo "══════════════════════════════════════════════════════"
echo "Phase 1: Starting local chains"
echo "══════════════════════════════════════════════════════"

# --- Start anvil ---
ANVIL_PID=""
if need_cmd anvil; then
    echo "Starting anvil..."
    anvil --host 127.0.0.1 --port 8545 --silent &>/tmp/anvil.log &
    ANVIL_PID=$!
    sleep 3
    if cast block-number --rpc-url http://127.0.0.1:8545 &>/dev/null; then
        _pass "anvil started"
    else
        _fail "anvil failed"
        kill "$ANVIL_PID" 2>/dev/null || true
        wait "$ANVIL_PID" 2>/dev/null || true
        ANVIL_PID=""
    fi
else
    _skip "anvil not available"
fi

# --- Start solana-test-validator ---
SOLANA_PID=""
SOLANA_VALIDATOR_UP=false
if $SOLANA_VALIDATOR_AVAILABLE; then
    echo "Starting solana-test-validator..."
    solana-test-validator \
        --reset \
        --limit-ledger-size 2000000 \
        --slots-per-epoch 64 \
        --rpc-port 8899 \
        --faucet-port "$SOLANA_FAUCET_PORT" \
        --quiet \
        &>/tmp/solana-validator.log &
    SOLANA_PID=$!

    # Wait for validator health check
    for i in $(seq 1 60); do
        if curl -s -X POST "http://localhost:8899" \
            -H "Content-Type: application/json" \
            -d '{"jsonrpc":"2.0","id":1,"method":"getHealth"}' | grep -q "ok" 2>/dev/null; then
            echo "  Solana validator ready after ${i}s"
            SOLANA_VALIDATOR_UP=true
            break
        fi
        sleep 2
    done

    if $SOLANA_VALIDATOR_UP; then
        # Set up payer keypair for test transactions
        PAYER_KEY="/tmp/solana-test-payer.json"
        solana-keygen new -o "$PAYER_KEY" --no-bip39-passphrase --force --silent 2>/dev/null
        solana config set --keypair "$PAYER_KEY" --url http://localhost:8899 &>/dev/null
        solana airdrop 100 --url http://localhost:8899 &>/dev/null
        sleep 1
        _pass "solana-test-validator started"
    else
        echo "  Solana validator logs:"; tail -3 /tmp/solana-validator.log
        _fail "solana-test-validator failed"
        pkill -P "$SOLANA_PID" 2>/dev/null || true
        kill "$SOLANA_PID" 2>/dev/null || true
        wait "$SOLANA_PID" 2>/dev/null || true
        SOLANA_PID=""
    fi
else
    _skip "solana-test-validator not available"
fi

# --- Start bitcoind via Docker ---
BTC_CONTAINER="rustplorer-bitcoin-e2e"
BTC_UP=false
if need_cmd docker; then
    echo "Starting bitcoind (regtest) via Docker..."
    docker rm -f "$BTC_CONTAINER" &>/dev/null || true
    docker run -d --name "$BTC_CONTAINER" \
        -p "$BTC_PORT":"$BTC_PORT" \
        lncm/bitcoind:v24.0 \
        -regtest=1 \
        -rpcuser="$BTC_RPCUSER" \
        -rpcpassword="$BTC_RPCPASS" \
        -rpcport="$BTC_PORT" \
        -rpcbind=0.0.0.0 \
        -rpcallowip=0.0.0.0/0 \
        -server=1 \
        -printtoconsole=1 \
        -fallbackfee=0.00001 \
        &>/tmp/btc-docker.log 2>&1

    for i in $(seq 1 30); do
        if docker exec "$BTC_CONTAINER" bitcoin-cli -regtest -rpcuser="$BTC_RPCUSER" -rpcpassword="$BTC_RPCPASS" -rpcport="$BTC_PORT" getblockchaininfo &>/dev/null; then
            BTC_UP=true
            break
        fi
        sleep 2
    done

    if $BTC_UP; then
        docker exec "$BTC_CONTAINER" bitcoin-cli -regtest -rpcuser="$BTC_RPCUSER" -rpcpassword="$BTC_RPCPASS" -rpcport="$BTC_PORT" createwallet "e2e" &>/dev/null || true
        docker exec "$BTC_CONTAINER" bitcoin-cli -regtest -rpcuser="$BTC_RPCUSER" -rpcpassword="$BTC_RPCPASS" -rpcport="$BTC_PORT" generatetoaddress 101 "$(docker exec "$BTC_CONTAINER" bitcoin-cli -regtest -rpcuser="$BTC_RPCUSER" -rpcpassword="$BTC_RPCPASS" -rpcport="$BTC_PORT" getnewaddress)" &>/dev/null
        _pass "bitcoind started"
    else
        echo "  bitcoind logs:"; tail -3 /tmp/btc-docker.log
        _fail "bitcoind failed"
    fi
else
    _skip "docker not available (bitcoind skipped)"
fi

# ============================================================
# Phase 2: Deploy ERC-20 + Send EVM Transfers
# ============================================================
echo ""
echo "══════════════════════════════════════════════════════"
echo "Phase 2: Deploy ERC-20 and send EVM transfers"
echo "══════════════════════════════════════════════════════"

ANVIL_RPC="http://127.0.0.1:8545"
SENDER_KEY="0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
ANVIL_TARGET="0x70997970c51812dc3a010c7d01b50e0d17dc79c8"
ETH_BLOCK_BEFORE=0
ETH_BLOCK_AFTER=0

# Deploy MockToken
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

contract FailToken {
    string public name = "FailToken";
    string public symbol = "FTK_FAIL";
    uint8 public decimals = 6;
    event Transfer(address indexed from, address indexed to, uint256 value);
    
    // Emits a Transfer log, but then intentionally reverts the transaction
    function transferAndRevert(address to, uint256 value) public {
        emit Transfer(msg.sender, to, value);
        revert("intentional EVM revert");
    }
}
SOL
cat > "$WORKDIR/foundry.toml" << 'TOML'
[profile.default]
src = "src"
out = "out"
libs = ["lib"]
TOML

if [ -n "$ANVIL_PID" ]; then
    forge build --root "$WORKDIR" &>/dev/null
    TOKEN_ADDR=$(forge create --rpc-url "$ANVIL_RPC" --private-key "$SENDER_KEY" --root "$WORKDIR" --broadcast src/MockToken.sol:MockToken 2>&1 | grep "Deployed to:" | awk '{print $3}')
    FEE_TOKEN_ADDR=$(forge create --rpc-url "$ANVIL_RPC" --private-key "$SENDER_KEY" --root "$WORKDIR" --broadcast src/MockToken.sol:FeeToken 2>&1 | grep "Deployed to:" | awk '{print $3}')
    echo "MockToken deployed: $TOKEN_ADDR"
    echo "FeeToken deployed: $FEE_TOKEN_ADDR"

    FAIL_TOKEN_ADDR=$(forge create --rpc-url "$ANVIL_RPC" --private-key "$SENDER_KEY" --root "$WORKDIR" --broadcast src/MockToken.sol:FailToken 2>&1 | grep "Deployed to:" | awk '{print $3}')
    echo "FailToken deployed: $FAIL_TOKEN_ADDR"

    # This call will fail and revert on-chain
    cast send "$FAIL_TOKEN_ADDR" "transferAndRevert(address,uint256)" "$ANVIL_TARGET" 50000000 --rpc-url "$ANVIL_RPC" --private-key "$SENDER_KEY" &>/dev/null || true

    _pass "ERC-20 contracts deployed"

    SNAPSHOT_ID=$(curl -s -X POST -H "Content-Type: application/json" \
        --data '{"jsonrpc":"2.0","method":"evm_snapshot","params":[],"id":1}' \
        "$ANVIL_RPC" | jq -r '.result')
    echo "Anvil pre-transfer snapshot: $SNAPSHOT_ID"

    # Send transfers
    ETH_BLOCK_BEFORE=$(cast block-number --rpc-url "$ANVIL_RPC")
    echo "Sending 1 ETH to $ANVIL_TARGET..."
    cast send --rpc-url "$ANVIL_RPC" --private-key "$SENDER_KEY" "$ANVIL_TARGET" --value 1ether &>/dev/null
    echo "Sending 50 MTK to $ANVIL_TARGET..."
    cast send "$TOKEN_ADDR" "transfer(address,uint256)" "$ANVIL_TARGET" 50000000 --rpc-url "$ANVIL_RPC" --private-key "$SENDER_KEY" &>/dev/null
    echo "Sending 100 FTK to $ANVIL_TARGET (90 FTK net after fee)..."
    cast send "$FEE_TOKEN_ADDR" "transfer(address,uint256)" "$ANVIL_TARGET" 100000000 --rpc-url "$ANVIL_RPC" --private-key "$SENDER_KEY" &>/dev/null
    sleep 2
    ETH_BLOCK_AFTER=$(cast block-number --rpc-url "$ANVIL_RPC")
    _pass "ETH + ERC-20 + fee-on-transfer transfers sent"
else
    _skip "anvil not available (EVM transfers skipped)"; ANVIL_TARGET=""; TOKEN_ADDR=""; FEE_TOKEN_ADDR=""; SNAPSHOT_ID=""
fi

# ============================================================
# Phase 3: Send Solana Transfers
# ============================================================
echo ""
echo "══════════════════════════════════════════════════════"
echo "Phase 3: Send Solana transfers"
echo "══════════════════════════════════════════════════════"

SOL_TARGET=""
SOL_SLOT_BEFORE=0
SOL_SLOT_AFTER=0
SPL_MINT=""

if $SOLANA_VALIDATOR_UP; then
    # Generate a dedicated target keypair for this test
    SOL_TARGET_KEY="/tmp/solana-e2e-target.json"
    solana-keygen new -o "$SOL_TARGET_KEY" --no-bip39-passphrase --force --silent 2>/dev/null
    SOL_TARGET=$(solana address -k "$SOL_TARGET_KEY" 2>/dev/null | tr -d '\n')
    echo "Solana target: $SOL_TARGET"

    SOL_SLOT_BEFORE=$(curl -s -X POST "http://localhost:8899" \
        -H "Content-Type: application/json" \
        -d '{"jsonrpc":"2.0","id":1,"method":"getSlot"}' | jq -r '.result')
    echo "Solana slot before: $SOL_SLOT_BEFORE"

    echo "Sending 2.5 SOL to target..."
    solana transfer --url http://localhost:8899 --allow-unfunded-recipient \
        -k /tmp/solana-test-payer.json "$SOL_TARGET" 2.5 &>/dev/null

    if $SOLANA_SPL_AVAILABLE; then
        echo "Creating SPL token mint..."
        SPL_MINT=$(spl-token create-token --decimals 9 --url http://localhost:8899 2>/dev/null | awk '/Creating token/ {print $3}')
        echo "SPL mint: $SPL_MINT"
        spl-token create-account "$SPL_MINT" --url http://localhost:8899 &>/dev/null
        spl-token mint "$SPL_MINT" 100 --url http://localhost:8899 &>/dev/null
        echo "Sending 15.5 SPL tokens to $SOL_TARGET..."
        spl-token transfer "$SPL_MINT" 15.5 "$SOL_TARGET" --url http://localhost:8899 --fund-recipient &>/dev/null
        _pass "SPL transfer sent"
    else
        _skip "spl-token not available (SPL transfers skipped)"
    fi

    SIG_SLOT=0
    for _ in $(seq 1 45); do
        sleep 1
        SIG_RESP=$(curl -s -X POST "http://localhost:8899" \
            -H "Content-Type: application/json" \
            -d '{"jsonrpc":"2.0","id":1,"method":"getSignaturesForAddress","params":["'$SOL_TARGET'",{"limit":10,"minContextSlot":'$SOL_SLOT_BEFORE',"commitment":"confirmed"}]}' 2>/dev/null)
        SIG_COUNT=$(echo "$SIG_RESP" | jq '.result | length' 2>/dev/null || echo 0)
        if [ "${SIG_COUNT:-0}" -gt 0 ]; then
            # Grab the highest slot from the returned signatures
            SIG_SLOT=$(echo "$SIG_RESP" | jq '[.result[].slot] | max' 2>/dev/null || echo 0)
        fi
        CUR_SLOT=$(curl -s -X POST "http://localhost:8899" \
            -H "Content-Type: application/json" \
            -d '{"jsonrpc":"2.0","id":1,"method":"getSlot","params":[{"commitment":"confirmed"}]}' | jq -r '.result')
        # Wait until signatures are visible AND the slot has advanced past our
        # starting point enough to comfortably cover the transaction slots.
        if [ "${SIG_COUNT:-0}" -gt 0 ] && [ "${CUR_SLOT:-0}" -gt "$((SOL_SLOT_BEFORE + 2))" ]; then
            break
        fi
    done

    # Use the max of getSlot and the actual signature slot, ensuring the scan
    # range covers the confirmed transaction even if getSlot lags slightly.
    SOL_SLOT_AFTER=$SIG_SLOT
    if [ "${CUR_SLOT:-0}" -gt "$SOL_SLOT_AFTER" ]; then
        SOL_SLOT_AFTER=$CUR_SLOT
    fi
    echo "Solana slot after: $SOL_SLOT_AFTER (sig_slot=$SIG_SLOT, cur_slot=${CUR_SLOT:-0})"
    _pass "SOL transfer sent"
else
    _skip "solana-test-validator not available (SOL transfers skipped)"
fi

# ============================================================
# Phase 4: Send BTC Transfer
# ============================================================
echo ""
echo "══════════════════════════════════════════════════════"
echo "Phase 4: Send BTC transfer"
echo "══════════════════════════════════════════════════════"

BTC_BLOCK_BEFORE=0
BTC_BLOCK_AFTER=0

if $BTC_UP; then
    _btc() { docker exec "$BTC_CONTAINER" bitcoin-cli -regtest -rpcuser="$BTC_RPCUSER" -rpcpassword="$BTC_RPCPASS" -rpcport="$BTC_PORT" "$@"; }
    BTC_TARGET_1=$(_btc getnewaddress "e2e-target-1" "bech32")
    BTC_TARGET_2=$(_btc getnewaddress "e2e-target-2" "bech32")
    BTC_BLOCK_BEFORE=$(_btc getblockcount)
    echo "Sending multi-output BTC transaction (0.12345678 to target 1, 0.05 to target 2)..."
    _btc sendmany "" "{\"$BTC_TARGET_1\":0.12345678,\"$BTC_TARGET_2\":0.05000000}" &>/dev/null
    _btc generatetoaddress 1 "$(_btc getnewaddress "miner")" &>/dev/null
    BTC_BLOCK_AFTER=$(_btc getblockcount)
    _pass "BTC transfer sent"
else
    _skip "bitcoind not available (BTC transfer skipped)"
fi

# ============================================================
# Phase 5: Build rustplorer
# ============================================================
echo ""
echo "══════════════════════════════════════════════════════"
echo "Phase 5: Build rustplorer"
echo "══════════════════════════════════════════════════════"

cd "$PROJECT_DIR"
cargo build -q 2>&1 || { echo "BUILD FAILED"; exit 1; }
_pass "rustplorer built"

# ============================================================
# Phase 6: Create test config (all chains)
# ============================================================
echo ""
echo "══════════════════════════════════════════════════════"
echo "Phase 6: Create test config"
echo "══════════════════════════════════════════════════════"

cat > "$CONFIG" << TOML
# ==========================================
# Test configuration for e2e
# ==========================================
[chains.anvil]
caip2 = "eip155:31337"
start_block = ${ETH_BLOCK_BEFORE:-0}
end_block = ${ETH_BLOCK_AFTER:-0}
rpc = [
    "http://127.0.0.1:8545",
]

  [chains.anvil.assets.ETH]
  contract = "native"
  decimals = 18

  [chains.anvil.assets.MTK]
  contract = "${TOKEN_ADDR:-0x0}"
  decimals = 6

  [chains.anvil.assets.FTK]
  contract = "${FEE_TOKEN_ADDR:-0x0}"
  decimals = 6

  [chains.anvil.assets.FTK_FAIL]
  contract = "${FAIL_TOKEN_ADDR:-0x0}"
  decimals = 6
TOML

# Append Solana if available
if $SOLANA_VALIDATOR_UP && [ -n "$SOL_TARGET" ]; then
cat >> "$CONFIG" << TOML

[chains.solana]
caip2 = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp"
start_block = $SOL_SLOT_BEFORE
end_block = $SOL_SLOT_AFTER
rpc = [
    "http://127.0.0.1:8899",
]

  [chains.solana.rpc_options]
  max_concurrent = 1
  delay_ms = 200

  [chains.solana.assets.SOL]
  contract = "native"
  decimals = 9
TOML

if $SOLANA_SPL_AVAILABLE && [ -n "$SPL_MINT" ]; then
cat >> "$CONFIG" << TOML

  [chains.solana.assets.SPL]
  contract = "$SPL_MINT"
  decimals = 9
TOML
fi
fi

# Append Bitcoin if available
if $BTC_UP && [ -n "$BTC_TARGET_1" ]; then
cat >> "$CONFIG" << TOML

[chains.bitcoin]
caip2 = "bip122:000000000019d6689c085ae165831e93"
start_block = $BTC_BLOCK_BEFORE
end_block = $BTC_BLOCK_AFTER
rpc = [
    "http://$BTC_RPCUSER:$BTC_RPCPASS@$BTC_HOST:$BTC_PORT",
]

  [chains.bitcoin.assets.BTC]
  contract = "native"
  decimals = 8
TOML
fi

# Build address list
echo "$ANVIL_TARGET" > "$ADDRS"
if [ -n "$SOL_TARGET" ]; then echo "$SOL_TARGET" >> "$ADDRS"; fi
if [ -n "$BTC_TARGET_1" ]; then echo "$BTC_TARGET_1" >> "$ADDRS"; fi
if [ -n "$BTC_TARGET_2" ]; then echo "$BTC_TARGET_2" >> "$ADDRS"; fi
_pass "test config created"

# ============================================================
# Phase 7: Test Deposit Detection (Single Scan)
# ============================================================
echo ""
echo "══════════════════════════════════════════════════════"
echo "Phase 7: Test deposit detection"
echo "══════════════════════════════════════════════════════"

SCAN_OUT=$(cargo run -q -- --config "$CONFIG" --addresses "$ADDRS" --format json 2>/dev/null)

# Verify EVM native
if [ -n "$ANVIL_TARGET" ]; then
    NATIVE_AMT=$(echo "$SCAN_OUT" | jq -r '.[] | select(.chain == "anvil" and .asset == "ETH") | .amount_clean' 2>/dev/null | head -1)
    if [ "$NATIVE_AMT" = "1" ]; then
        _pass "EVM native ETH: $NATIVE_AMT ETH detected"
    else
        _fail "EVM native ETH: expected 1, got '${NATIVE_AMT:-none}'"
    fi

    # Verify ERC-20
    MTK_AMT=$(echo "$SCAN_OUT" | jq -r '.[] | select(.chain == "anvil" and .asset == "MTK") | .amount_clean' 2>/dev/null | head -1)
    if [ "$MTK_AMT" = "50" ]; then
        _pass "EVM ERC-20 MTK: $MTK_AMT MTK detected"
    else
        _fail "EVM ERC-20 MTK: expected 50, got '${MTK_AMT:-none}'"
    fi

    FTK_AMT=$(echo "$SCAN_OUT" | jq -r '.[] | select(.chain == "anvil" and .asset == "FTK") | .amount_clean' 2>/dev/null | head -1)
    if [ "$FTK_AMT" = "90" ]; then
        _pass "EVM FeeToken FTK: $FTK_AMT FTK net detected"
    else
        _fail "EVM FeeToken FTK: expected 90, got '${FTK_AMT:-none}'"
    fi

    FAIL_AMT=$(echo "$SCAN_OUT" | jq -r '.[] | select(.chain == "anvil" and .asset == "FTK_FAIL") | .amount_clean' 2>/dev/null | head -1)
    if [ -z "$FAIL_AMT" ] || [ "$FAIL_AMT" = "null" ]; then
        _pass "EVM failed transaction: reverted transfer correctly ignored"
    else
        _fail "EVM failed transaction: detected reverted transfer ($FAIL_AMT FTK_FAIL)"
    fi
fi

# Verify Solana
if $SOLANA_VALIDATOR_UP && [ -n "$SOL_TARGET" ]; then
    SOL_AMT=$(echo "$SCAN_OUT" | jq -r '.[] | select(.chain == "solana" and .asset == "SOL" and .to_address == "'$SOL_TARGET'") | .amount_clean' 2>/dev/null | head -1)
    if [ "$SOL_AMT" = "2.5" ]; then
        _pass "Solana native SOL: $SOL_AMT SOL detected"
    else
        _fail "Solana native SOL: expected 2.5, got '${SOL_AMT:-none}'"
    fi

    if $SOLANA_SPL_AVAILABLE && [ -n "$SPL_MINT" ]; then
        SPL_AMT=$(echo "$SCAN_OUT" | jq -r '.[] | select(.chain == "solana" and .asset == "SPL" and .to_address == "'$SOL_TARGET'") | .amount_clean' 2>/dev/null | head -1)
        if [ "$SPL_AMT" = "15.5" ]; then
            _pass "Solana SPL token: $SPL_AMT SPL detected"
        else
            _fail "Solana SPL token: expected 15.5, got '${SPL_AMT:-none}'"
        fi
    fi
fi

if [ -n "${ANVIL_PID:-}" ] && [ -n "${SNAPSHOT_ID:-}" ]; then
    echo "Simulating EVM reorg via anvil snapshot revert..."
    curl -s -X POST -H "Content-Type: application/json" \
        --data '{"jsonrpc":"2.0","method":"evm_revert","params":["'$SNAPSHOT_ID'"],"id":1}' \
        "$ANVIL_RPC" &>/dev/null
    cast send --rpc-url "$ANVIL_RPC" --private-key "$SENDER_KEY" "$ANVIL_TARGET" --value 2ether &>/dev/null
    # The snapshot was taken before the original ERC-20 transfers. Re-send the
    # token deposits on the canonical post-reorg chain so later CLI/API JSON
    # output still demonstrates all configured Anvil assets, not only Native.
    cast send "$TOKEN_ADDR" "transfer(address,uint256)" "$ANVIL_TARGET" 50000000 --rpc-url "$ANVIL_RPC" --private-key "$SENDER_KEY" &>/dev/null
    cast send "$FEE_TOKEN_ADDR" "transfer(address,uint256)" "$ANVIL_TARGET" 100000000 --rpc-url "$ANVIL_RPC" --private-key "$SENDER_KEY" &>/dev/null
    sleep 2
    REORG_SCAN=$(cargo run -q -- --config "$CONFIG" --addresses "$ADDRS" --format json 2>/dev/null)
    REORG_AMT=$(echo "$REORG_SCAN" | jq -r '.[] | select(.chain == "anvil" and .asset == "ETH") | .amount_clean' 2>/dev/null | head -1)
    if [ "$REORG_AMT" = "2" ]; then
        _pass "EVM reorg: canonical 2 ETH deposit detected"
    else
        _fail "EVM reorg: expected 2 ETH, got '${REORG_AMT:-none}'"
    fi
fi

# Verify BTC
if $BTC_UP && [ -n "$BTC_TARGET_1" ] && [ -n "$BTC_TARGET_2" ]; then
    BTC_RAW_1=$(echo "$SCAN_OUT" | jq -r '.[] | select(.chain == "bitcoin" and .to_address == "'$BTC_TARGET_1'") | .amount_raw' 2>/dev/null | head -1)
    BTC_CLEAN_1=$(echo "$SCAN_OUT" | jq -r '.[] | select(.chain == "bitcoin" and .to_address == "'$BTC_TARGET_1'") | .amount_clean' 2>/dev/null | head -1)

    BTC_RAW_2=$(echo "$SCAN_OUT" | jq -r '.[] | select(.chain == "bitcoin" and .to_address == "'$BTC_TARGET_2'") | .amount_raw' 2>/dev/null | head -1)
    BTC_CLEAN_2=$(echo "$SCAN_OUT" | jq -r '.[] | select(.chain == "bitcoin" and .to_address == "'$BTC_TARGET_2'") | .amount_clean' 2>/dev/null | head -1)

    if [ "$BTC_RAW_1" = "12345678" ] && [ "$BTC_RAW_2" = "5000000" ]; then
        _pass "BTC multi-output precision: raw targets resolved correctly (12345678 and 5000000 sats)"
    else
        _fail "BTC multi-output precision raw: expected 12345678/5000000, got '${BTC_RAW_1:-none}'/'${BTC_RAW_2:-none}'"
    fi

    if [ "$BTC_CLEAN_1" = "0.12345678" ] && [ "$BTC_CLEAN_2" = "0.05" ]; then
        _pass "BTC multi-output precision: clean targets resolved correctly (0.12345678 and 0.05 BTC)"
    else
        _fail "BTC multi-output precision clean: expected 0.12345678/0.05, got '${BTC_CLEAN_1:-none}'/'${BTC_CLEAN_2:-none}'"
    fi
fi

# ============================================================
# Phase 8: Test CSV output
# ============================================================
echo ""
echo "══════════════════════════════════════════════════════"
echo "Phase 8: Test CSV output"
echo "══════════════════════════════════════════════════════"

CSV_OUT="$TESTDIR/deposits.csv"
cargo run -q -- --config "$CONFIG" --addresses "$ADDRS" --format csv --output "$CSV_OUT" 2>/dev/null
if head -1 "$CSV_OUT" | rg -q "chain,asset,from_address,to_address"; then
    _pass "CSV header correct"
else
    _fail "CSV header missing"
fi
CSV_LINES=$(wc -l < "$CSV_OUT" | tr -d ' ')
if [ "$CSV_LINES" -gt 1 ]; then
    _pass "CSV has $CSV_LINES lines (header + data)"
else
    _fail "CSV has no data rows"
fi

# ============================================================
# Phase 9: Start Daemon + Test All API Endpoints
# ============================================================
echo ""
echo "══════════════════════════════════════════════════════"
echo "Phase 9: Start daemon + test API endpoints"
echo "══════════════════════════════════════════════════════"

RUST_LOG=info cargo run -q -- --config "$CONFIG" --addresses "$ADDRS" --api-port "$API_PORT" --watch --interval 5 &> "$LOG" &

DAEMON_PID=$!
sleep 3

API="http://127.0.0.1:$API_PORT"

# Wait for API to respond
for i in $(seq 1 20); do
    if curl -s "$API/v1/config" | jq -e '.data.chains' &>/dev/null; then break; fi
    sleep 1
done

stop_daemon() {
    if [ -n "${DAEMON_PID:-}" ] && kill -0 "$DAEMON_PID" 2>/dev/null; then
        kill -INT "$DAEMON_PID" 2>/dev/null || true
        sleep 2
        kill -9 "$DAEMON_PID" 2>/dev/null || true
    fi
}

# ---------- GET / ----------
DASH_OK=false
for i in $(seq 1 10); do
    DASH=$(curl -s "$API/" 2>/dev/null)
    if grep -qi "Rustplorer" <<< "$DASH"; then
        DASH_OK=true; break
    fi
    sleep 1
done
if $DASH_OK; then
    _pass "GET / dashboard served"
else
    _fail "GET / dashboard missing"
fi

# ---------- GET /v1/config ----------
if curl -s "$API/v1/config" | jq -e '.data.chains.anvil.caip2 == "eip155:31337"' &>/dev/null; then
    _pass "GET /v1/config correct"
else
    _fail "GET /v1/config incorrect"
fi

# ---------- GET /v1/addresses ----------
ADDR_COUNT=$(curl -s "$API/v1/addresses" | jq '.meta.total' 2>/dev/null)
if [ "${ADDR_COUNT:-0}" -gt 0 ]; then
    _pass "GET /v1/addresses: $ADDR_COUNT addresses"
else
    _fail "GET /v1/addresses: no addresses"
fi

# ---------- POST /v1/addresses (single) ----------
curl -s -X POST "$API/v1/addresses" -H "Content-Type: application/json" \
    -d '{"address": "0xDDdDddDdDdDDDDDDDDDDDDDDdDDdddDDdDDDDDDDD"}' | jq -e '.data.added == 1' &>/dev/null && \
    _pass "POST /v1/addresses (single) 201" || _fail "POST /v1/addresses (single)"

# ---------- POST /v1/addresses (batch) ----------
curl -s -X POST "$API/v1/addresses" -H "Content-Type: application/json" \
    -d '{"addresses": ["0x1111222233334444555566667777888899990000", "0x22223333444455556666777788889999aaaabbbb"]}' | jq -e '.data.added == 2' &>/dev/null && \
    _pass "POST /v1/addresses (batch) 201" || _fail "POST /v1/addresses (batch)"

# ---------- DELETE /v1/addresses/:addr ----------
curl -s -X DELETE "$API/v1/addresses/0x1111222233334444555566667777888899990000" | jq -e '.data.removed == 1' &>/dev/null && \
    _pass "DELETE /v1/addresses/:addr" || _fail "DELETE /v1/addresses/:addr"

# ---------- GET /v1/deposits ----------
DEPO_COUNT=0
for _ in $(seq 1 20); do
    DEPO_COUNT=$(curl -s "$API/v1/deposits" | jq '.meta.total' 2>/dev/null)
    [ "${DEPO_COUNT:-0}" -gt 0 ] && break
    sleep 1
done
if [ "${DEPO_COUNT:-0}" -gt 0 ]; then
    _pass "GET /v1/deposits: $DEPO_COUNT deposits"
else
    _fail "GET /v1/deposits: empty"
fi

# ---------- GET /v1/balances ----------
BAL_COUNT=$(curl -s "$API/v1/balances" | jq '.meta.total' 2>/dev/null)
if [ "${BAL_COUNT:-0}" -gt 0 ]; then
    _pass "GET /v1/balances: $BAL_COUNT balances"
else
    _fail "GET /v1/balances: empty"
fi

# ---------- POST /v1/chains ----------
curl -s -X POST "$API/v1/chains" -H "Content-Type: application/json" \
    -d '{"name":"polygon","caip2":"eip155:137","rpc":["https://polygon-rpc.com"],"start_block":50000000}' | jq -e '.data.name == "polygon"' &>/dev/null && \
    _pass "POST /v1/chains 201" || _fail "POST /v1/chains"

# Verify chain appears
curl -s "$API/v1/config" | jq -e '.data.chains.polygon.caip2 == "eip155:137"' &>/dev/null && \
    _pass "Chain polygon in config" || _fail "Chain polygon missing"

# ---------- DELETE /v1/chains ----------
curl -s -X DELETE "$API/v1/chains/polygon" | jq -e '.data.removed == "polygon"' &>/dev/null && \
    _pass "DELETE /v1/chains 200" || _fail "DELETE /v1/chains"

# ---------- POST /v1/assets ----------
curl -s -X POST "$API/v1/assets" -H "Content-Type: application/json" \
    -d '{"chain":"anvil","name":"DAI","contract":"0x6B175474E89094C44Da98b954EedeAC495271d0F","decimals":18}' | jq -e '.data.name == "DAI"' &>/dev/null && \
    _pass "POST /v1/assets 201" || _fail "POST /v1/assets"

# Verify asset nested under chain
curl -s "$API/v1/config" | jq -e '.data.chains.anvil.assets.DAI.decimals == 18' &>/dev/null && \
    _pass "Asset DAI nested under anvil" || _fail "Asset DAI missing"

# ---------- DELETE /v1/assets/:chain/:asset ----------
curl -s -X DELETE "$API/v1/assets/anvil/DAI" | jq -e '.data.removed.chain == "anvil"' &>/dev/null && \
    _pass "DELETE /v1/assets/:chain/:asset 200" || _fail "DELETE /v1/assets"

# ---------- API Error handling ----------
# 400 - bad request (POST without addresses field)
HTTP_400=$(curl -s -o /dev/null -w "%{http_code}" -X POST "$API/v1/addresses" -H "Content-Type: application/json" -d '{"invalid":"data"}' 2>/dev/null)
if [ "$HTTP_400" = "400" ]; then
    _pass "API 400 Bad Request"
else
    _fail "API 400 Bad Request (got HTTP $HTTP_400)"
fi

# 404 - not found
HTTP_404=$(curl -s -o /dev/null -w "%{http_code}" -X DELETE "$API/v1/chains/nonexistent" 2>/dev/null)
if [ "$HTTP_404" = "404" ]; then
    _pass "API 404 Not Found"
else
    _fail "API 404 Not Found (got HTTP $HTTP_404)"
fi

# 409 - conflict (add existing chain)
HTTP_409=$(curl -s -o /dev/null -w "%{http_code}" -X POST "$API/v1/chains" -H "Content-Type: application/json" \
    -d '{"name":"anvil","caip2":"eip155:31337","rpc":["http://127.0.0.1:8545"]}' 2>/dev/null)
if [ "$HTTP_409" = "409" ]; then
    _pass "API 409 Conflict"
else
    _fail "API 409 Conflict (got HTTP $HTTP_409)"
fi

# ============================================================
# Phase 10: Test CLI Commands
# ============================================================
echo ""
echo "══════════════════════════════════════════════════════"
echo "Phase 10: Test CLI commands"
echo "══════════════════════════════════════════════════════"

# Copy config for CLI tests
CLI_CFG="$TESTDIR/cli-test.toml"
CLI_ADDR="$TESTDIR/cli-addresses.txt"
cp "$CONFIG" "$CLI_CFG"
echo "0x70997970c51812dc3a010c7d01b50e0d17dc79c8" > "$CLI_ADDR"

# --add-address
cargo run -q -- -a "$CLI_ADDR" --add-address "0xAbCdEf00112233445566778899aAbBcCdDeEfF00" 2>/dev/null
cat "$CLI_ADDR" | tr '[:upper:]' '[:lower:]' | rg -q "0xabcdef00112233445566778899aabbccddeeff00" && \
    _pass "CLI --add-address" || _fail "CLI --add-address"

# --remove-address
cargo run -q -- -a "$CLI_ADDR" --remove-address "0xabcdef00112233445566778899aabbccddeeff00" 2>/dev/null
rg -q "0xabcdef00112233445566778899aabbccddeeff00" "$CLI_ADDR" && \
    _fail "CLI --remove-address (still present)" || _pass "CLI --remove-address"

# --add-chain
cargo run -q -- -c "$CLI_CFG" -a "$CLI_ADDR" --add-chain "optimism,eip155:10,https://mainnet.optimism.io" 2>/dev/null
rg -q '\[chains.optimism\]' "$CLI_CFG" && _pass "CLI --add-chain" || _fail "CLI --add-chain"

# --remove-chain
cargo run -q -- -c "$CLI_CFG" -a "$CLI_ADDR" --remove-chain "optimism" 2>/dev/null
rg -q '\[chains.optimism\]' "$CLI_CFG" && _fail "CLI --remove-chain (still present)" || _pass "CLI --remove-chain"

# --add-asset
cargo run -q -- -c "$CLI_CFG" -a "$CLI_ADDR" --add-asset "anvil,USDT,0xdAC17F958D2ee523a2206206994597C13D831ec7,6" 2>/dev/null
rg -q '\[chains.anvil.assets.USDT\]' "$CLI_CFG" && _pass "CLI --add-asset" || _fail "CLI --add-asset"

# --remove-asset
cargo run -q -- -c "$CLI_CFG" -a "$CLI_ADDR" --remove-asset "anvil,USDT" 2>/dev/null
rg -q '\[chains.anvil.assets.USDT\]' "$CLI_CFG" && _fail "CLI --remove-asset (still present)" || _pass "CLI --remove-asset"

# --verbose (no crash)
cargo run -q -- -c "$CONFIG" -a "$ADDRS" --verbose 2>/dev/null && \
    _pass "CLI --verbose" || _fail "CLI --verbose"

# --help
cargo run -q -- --help 2>/dev/null | rg -q "rustplorer" && \
    _pass "CLI --help" || _fail "CLI --help"

# --version
cargo run -q -- --version 2>/dev/null | rg -q "rustplorer" && \
    _pass "CLI --version" || _fail "CLI --version"

# ============================================================
# Phase 11: Run solana_local integration tests
# ============================================================
if $SOLANA_VALIDATOR_UP; then
    echo ""
    echo "══════════════════════════════════════════════════════"
    echo "Phase 11: Run solana_local integration tests"
    echo "══════════════════════════════════════════════════════"

    cd "$PROJECT_DIR"
    if RUST_LOG=info cargo test --test solana_local -- --nocapture 2>&1; then
        _pass "solana_local integration tests"
    else
        _fail "solana_local integration tests"
    fi
else
    _skip "solana-test-validator not available (solana_local tests skipped)"
fi

# ============================================================
# Phase 12: Graceful Shutdown
# ============================================================
echo ""
echo "══════════════════════════════════════════════════════"
echo "Phase 12: Graceful shutdown"
echo "══════════════════════════════════════════════════════"

kill -INT "$DAEMON_PID" 2>/dev/null || true
sleep 3
if kill -0 "$DAEMON_PID" 2>/dev/null; then
    _fail "daemon still running after SIGINT"
    kill -9 "$DAEMON_PID" 2>/dev/null || true
else
    _pass "graceful shutdown (SIGINT)"
fi
# Check log - trace messages may be on stderr, give time to flush
sleep 1
if grep -qi "shutdown" "$LOG" 2>/dev/null; then
    _pass "shutdown message logged"
else
    _fail "shutdown message missing from log"
fi

DAEMON_PID=""  # reset so cleanup trap doesn't double-kill

# ============================================================
# Phase 13: Docker Build + API Test
# ============================================================
echo ""
echo "══════════════════════════════════════════════════════"
echo "Phase 13: Docker build + run + API"
echo "══════════════════════════════════════════════════════"

cd "$PROJECT_DIR"
echo "Building Docker image..."
if docker build -t rustplorer:e2e -f Dockerfile . &>/tmp/docker-build.log; then
    _pass "docker build"
else
    _fail "docker build"; tail -5 /tmp/docker-build.log
fi

# Create a Docker-specific config that won't try to contact local RPCs
DOCKER_CFG="$TESTDIR/docker-config.toml"
DOCKER_ADDR="$TESTDIR/docker-addresses.txt"
cat > "$DOCKER_CFG" << TOML
[chains.ethereum]
caip2 = "eip155:1"
rpc = [
    "https://ethereum.publicnode.com",
]
start_block = 22000000
end_block = 22000001
TOML
echo "0x70997970c51812dc3a010c7d01b50e0d17dc79c8" > "$DOCKER_ADDR"

# Run container with test config
echo "Starting rustplorer container..."
docker rm -f rustplorer-docker-e2e &>/dev/null || true
docker run -d --name rustplorer-docker-e2e \
    -p 3301:3301 \
    -v "$DOCKER_CFG:/app/Config.toml" \
    -v "$DOCKER_ADDR:/app/addresses.txt" \
    rustplorer:e2e \
    --config /app/Config.toml \
    --addresses /app/addresses.txt \
    --api-port 3301 \
    --host 0.0.0.0 \
    --watch \
    --interval 30 &>/tmp/docker-run.log

sleep 4

DOCKER_API="http://127.0.0.1:3301"
# Wait for container to start
DOCKER_UP=false
for i in $(seq 1 15); do
    if curl -s -o /dev/null -w "%{http_code}" "$DOCKER_API/" 2>/dev/null | rg -q "200"; then
        DOCKER_UP=true; break
    fi
    if ! docker ps --format '{{.Names}}' | rg -q "rustplorer-docker-e2e"; then
        echo "  Container exited:"; docker logs rustplorer-docker-e2e 2>&1 | tail -5
        break
    fi
    sleep 2
done

if $DOCKER_UP; then
    _pass "Docker API: dashboard served"
else
    _fail "Docker API: dashboard not served"
fi
if curl -s "$DOCKER_API/v1/config" | jq -e '.data.chains' &>/dev/null; then
    _pass "Docker API: /v1/config"
else
    _fail "Docker API: /v1/config"
fi
if curl -s "$DOCKER_API/v1/addresses" | jq -e '.meta' &>/dev/null; then
    _pass "Docker API: /v1/addresses"
else
    _fail "Docker API: /v1/addresses"
fi

docker stop rustplorer-docker-e2e &>/dev/null
docker rm rustplorer-docker-e2e &>/dev/null

# ============================================================
# Cleanup
# ============================================================
echo ""
echo "══════════════════════════════════════════════════════"
echo "Cleanup"
echo "══════════════════════════════════════════════════════"

cleanup_chains
rm -rf "$TESTDIR" "$WORKDIR"
_pass "cleanup complete"

# ============================================================
# Summary
# ============================================================
echo ""
echo "══════════════════════════════════════════════════════"
echo "RESULTS: $PASS PASS, $FAIL FAIL, $SKIP SKIP"
echo "══════════════════════════════════════════════════════"
if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
echo "All e2e tests passed!"
