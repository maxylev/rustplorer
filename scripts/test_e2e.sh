#!/usr/bin/env bash
set -euo pipefail

echo "=== rustplorer E2E test suite ==="

need_cmd() {
    if ! command -v "$1" &>/dev/null; then
        echo "Error: $1 not found. $2"
        exit 1
    fi
}

need_cmd anvil "Install Foundry: https://getfoundry.sh"
need_cmd solana-test-validator "Install Solana CLI: https://docs.solana.com/cli/install-solana-cli"
need_cmd cast "Install Foundry: https://getfoundry.sh"
need_cmd forge "Install Foundry: https://getfoundry.sh"

cleanup() {
    echo ""
    echo "Stopping local chains..."
    pkill -x anvil 2>/dev/null || true
    pkill -x solana-test-validator 2>/dev/null || true
}
trap cleanup EXIT

# --- Start anvil ---
echo "Starting anvil..."
anvil --host 127.0.0.1 --port 8545 --silent &
sleep 2

cast block-number --rpc-url http://127.0.0.1:8545 || {
    echo "Error: anvil failed to start"
    exit 1
}

# --- Deploy ERC20 + send transfers ---
echo "Deploying MockToken on anvil..."
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
    constructor() {
        totalSupply = 1000000 * 10**6;
        balanceOf[msg.sender] = totalSupply;
        emit Transfer(address(0), msg.sender, totalSupply);
    }
    function transfer(address to, uint256 value) public returns (bool) {
        require(balanceOf[msg.sender] >= value, "insufficient");
        balanceOf[msg.sender] -= value;
        balanceOf[to] += value;
        emit Transfer(msg.sender, to, value);
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

forge build --root "$WORKDIR" 2>&1 | tail -1

DEPLOY_OUT=$(forge create --rpc-url http://127.0.0.1:8545 \
    --private-key 0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80 \
    --root "$WORKDIR" --broadcast src/MockToken.sol:MockToken 2>&1)

TOKEN_ADDR=$(echo "$DEPLOY_OUT" | grep "Deployed to:" | awk '{print $3}')
echo "MockToken deployed at: $TOKEN_ADDR"

# Send native ETH transfer
echo "Sending 1 ETH..."
cast send --rpc-url http://127.0.0.1:8545 \
    --private-key 0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80 \
    0x70997970C51812dc3A010C7d01b50e0d17dc79C8 --value 1ether 2>&1 | grep -q "success" && echo "  OK"

# Send ERC20 transfer (50 tokens)
echo "Sending 50 MTK..."
cast send "$TOKEN_ADDR" "transfer(address,uint256)" \
    0x70997970C51812dc3A010C7d01b50e0d17dc79C8 50000000 \
    --rpc-url http://127.0.0.1:8545 \
    --private-key 0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80 2>&1 | grep -q "success" && echo "  OK"

# --- Start Solana validator ---
echo ""
echo "Starting solana-test-validator..."
solana-test-validator --reset --quiet --rpc-port 8899 &
sleep 15

solana slot --url http://localhost:8899 || {
    echo "Error: solana-test-validator failed to start"
    exit 1
}

# Transfer SOL
RECEIVER="3zCGKxMK3JHNUMtHbticPoDvoRbUgzY65ayoHMWZwZE2"
echo "Sending 2.5 SOL..."
solana transfer --url http://localhost:8899 --allow-unfunded-recipient \
    "$RECEIVER" 2.5 2>&1 | grep -q "Signature" && echo "  OK"

sleep 2

# --- Build and run tests ---
echo ""
echo "Building rustplorer..."
cargo build 2>&1 | tail -1

echo ""
echo "Running unit + mock tests..."
cargo test 2>&1 | tail -5

echo ""
echo "Running E2E tests against local chains..."
cargo test --test e2e_test -- --ignored --nocapture 2>&1

echo ""
echo "=== All tests passed ==="
