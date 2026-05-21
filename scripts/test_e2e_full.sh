#!/usr/bin/env bash
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

TESTDIR="/tmp/rustplorer-e2e-test"
rm -rf "$TESTDIR" && mkdir -p "$TESTDIR"

CONFIG="$TESTDIR/Config.toml"
ADDRS="$TESTDIR/addresses.txt"
API_PORT="3300"
LOG="$TESTDIR/daemon.log"
BTC_HOST="127.0.0.1"
BTC_PORT="18443"
BTC_RPCUSER="rpcuser"
BTC_RPCPASS="rpcpassword"

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
        _fail "anvil failed"; ANVIL_PID=""
    fi
else
    _skip "anvil not available"
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
# Phase 2: Deploy ERC-20 + Send Transfers
# ============================================================
echo ""
echo "══════════════════════════════════════════════════════"
echo "Phase 2: Deploy ERC-20 and send transfers"
echo "══════════════════════════════════════════════════════"

ANVIL_RPC="http://127.0.0.1:8545"
SENDER_KEY="0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
ANVIL_TARGET="0x70997970c51812dc3a010c7d01b50e0d17dc79c8"

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
    echo "MockToken deployed: $TOKEN_ADDR"
    _pass "ERC-20 deployed"

    # Send transfers
    ETH_BLOCK_BEFORE=$(cast block-number --rpc-url "$ANVIL_RPC")
    echo "Sending 1 ETH to $ANVIL_TARGET..."
    cast send --rpc-url "$ANVIL_RPC" --private-key "$SENDER_KEY" "$ANVIL_TARGET" --value 1ether &>/dev/null
    echo "Sending 50 MTK to $ANVIL_TARGET..."
    cast send "$TOKEN_ADDR" "transfer(address,uint256)" "$ANVIL_TARGET" 50000000 --rpc-url "$ANVIL_RPC" --private-key "$SENDER_KEY" &>/dev/null
    sleep 2
    ETH_BLOCK_AFTER=$(cast block-number --rpc-url "$ANVIL_RPC")
    _pass "ETH + ERC-20 transfers sent"
else
    _skip "anvil not available (EVM transfers skipped)"; ANVIL_TARGET=""
fi

# --- Send BTC transfer ---
BTC_TARGET=""
if $BTC_UP; then
    _btc() { docker exec "$BTC_CONTAINER" bitcoin-cli -regtest -rpcuser="$BTC_RPCUSER" -rpcpassword="$BTC_RPCPASS" -rpcport="$BTC_PORT" "$@"; }
    BTC_TARGET=$(_btc getnewaddress "e2e-target")
    BTC_BLOCK_BEFORE=$(_btc getblockcount)
    echo "Sending 0.12345678 BTC to $BTC_TARGET..."
    _btc sendtoaddress "$BTC_TARGET" 0.12345678 &>/dev/null
    _btc generatetoaddress 1 "$(_btc getnewaddress "miner")" &>/dev/null
    BTC_BLOCK_AFTER=$(_btc getblockcount)
    _pass "BTC transfer sent"
else
    _skip "bitcoind not available (BTC transfer skipped)"
fi

# ============================================================
# Phase 3: Build rustplorer
# ============================================================
echo ""
echo "══════════════════════════════════════════════════════"
echo "Phase 3: Build rustplorer"
echo "══════════════════════════════════════════════════════"

cd /Users/llama/Developer/rustplorer
cargo build -q 2>&1 || { echo "BUILD FAILED"; exit 1; }
_pass "rustplorer built"

# ============================================================
# Phase 4: Create test config
# ============================================================
echo ""
echo "══════════════════════════════════════════════════════"
echo "Phase 4: Create test config"
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

  [chains.anvil.assets.ETH_NATIVE]
  contract = "native"
  decimals = 18

  [chains.anvil.assets.MTK]
  contract = "${TOKEN_ADDR:-0x0}"
  decimals = 6
TOML

if $BTC_UP && [ -n "$BTC_TARGET" ]; then
cat >> "$CONFIG" << TOML

[chains.bitcoin]
caip2 = "bip122:000000000019d6689c085ae165831e93"
start_block = $BTC_BLOCK_BEFORE
end_block = $BTC_BLOCK_AFTER
rpc = [
    "http://$BTC_RPCUSER:$BTC_RPCPASS@$BTC_HOST:$BTC_PORT",
]

  [chains.bitcoin.assets.BTC_NATIVE]
  contract = "native"
  decimals = 8
TOML
fi

echo "$ANVIL_TARGET" > "$ADDRS"
echo "$BTC_TARGET" >> "$ADDRS"
_pass "test config created"

# ============================================================
# Phase 5: Test Deposit Detection (Single Scan)
# ============================================================
echo ""
echo "══════════════════════════════════════════════════════"
echo "Phase 5: Test deposit detection"
echo "══════════════════════════════════════════════════════"

SCAN_OUT=$(cargo run -q -- --config "$CONFIG" --addresses "$ADDRS" --format json 2>/dev/null)

# Verify EVM native
if [ -n "$ANVIL_TARGET" ]; then
    NATIVE_AMT=$(echo "$SCAN_OUT" | jq -r '.[] | select(.chain == "anvil" and .asset == "Native") | .amount_clean' 2>/dev/null | head -1)
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
fi

# Verify BTC
if $BTC_UP && [ -n "$BTC_TARGET" ]; then
    BTC_RAW=$(echo "$SCAN_OUT" | jq -r '.[] | select(.chain == "bitcoin" and .asset == "Native") | .amount_raw' 2>/dev/null | head -1)
    BTC_CLEAN=$(echo "$SCAN_OUT" | jq -r '.[] | select(.chain == "bitcoin" and .asset == "Native") | .amount_clean' 2>/dev/null | head -1)
    if [ "$BTC_RAW" = "12345678" ]; then
        _pass "BTC precision: raw=$BTC_RAW sats"
    else
        _fail "BTC precision raw: expected 12345678, got '${BTC_RAW:-none}'"
    fi
    if [ "$BTC_CLEAN" = "0.12345678" ]; then
        _pass "BTC precision: clean=$BTC_CLEAN BTC"
    else
        _fail "BTC precision clean: expected 0.12345678, got '${BTC_CLEAN:-none}'"
    fi
fi

# ============================================================
# Phase 6: Test CSV output
# ============================================================
echo ""
echo "══════════════════════════════════════════════════════"
echo "Phase 6: Test CSV output"
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
# Phase 7: Start Daemon + Test All API Endpoints
# ============================================================
echo ""
echo "══════════════════════════════════════════════════════"
echo "Phase 7: Start daemon + test API endpoints"
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
trap "stop_daemon; cleanup_chains" EXIT
cleanup_chains() {
    docker rm -f "$BTC_CONTAINER" 2>/dev/null || true
    if [ -n "${ANVIL_PID:-}" ]; then kill "$ANVIL_PID" 2>/dev/null || true; fi
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
DEPO_COUNT=$(curl -s "$API/v1/deposits" | jq '.meta.total' 2>/dev/null)
if [ "${DEPO_COUNT:-0}" -gt 0 ]; then
    _pass "GET /v1/deposits: $DEPO_COUNT deposits"
else
    _fail "GET /v1/deposits: empty"
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
# Phase 8: Test CLI Commands
# ============================================================
echo ""
echo "══════════════════════════════════════════════════════"
echo "Phase 8: Test CLI commands"
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
# Phase 9: Graceful Shutdown
# ============================================================
echo ""
echo "══════════════════════════════════════════════════════"
echo "Phase 9: Graceful shutdown"
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

# ============================================================
# Phase 10: Docker Build + API Test
# ============================================================
echo ""
echo "══════════════════════════════════════════════════════"
echo "Phase 10: Docker build + run + API"
echo "══════════════════════════════════════════════════════"

cd /Users/llama/Developer/rustplorer
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

stop_daemon
docker rm -f "$BTC_CONTAINER" &>/dev/null || true
if [ -n "${ANVIL_PID:-}" ]; then kill "$ANVIL_PID" 2>/dev/null || true; fi
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
