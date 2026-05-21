#!/usr/bin/env bash
set -euo pipefail

echo "=== rustplorer local Solana integration test ==="

if ! command -v solana-test-validator &>/dev/null; then
    echo "Error: solana-test-validator not found."
    echo "Install: sh -c \"\$(curl -sSfL https://release.anza.xyz/stable/install)\""
    exit 1
fi

cleanup() {
    if [ -n "${VALIDATOR_PID:-}" ]; then
        echo "Stopping solana-test-validator (PID: $VALIDATOR_PID)..."
        kill "$VALIDATOR_PID" 2>/dev/null || true
        wait "$VALIDATOR_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT

echo "Starting solana-test-validator with extended history..."

solana-test-validator \
    --reset \
    --limit-ledger-size 2000000 \
    --slots-per-epoch 64 \
    --rpc-port 8899 \
    &>/tmp/solana-validator.log &
VALIDATOR_PID=$!

echo "Waiting for validator to be ready..."
for i in $(seq 1 120); do
    if curl -s -X POST "http://localhost:8899" \
        -H "Content-Type: application/json" \
        -d '{"jsonrpc":"2.0","id":1,"method":"getHealth"}' | grep -q "ok" 2>/dev/null; then
        echo "Validator ready after ${i}s"
        break
    fi
    sleep 1
done

echo "Setting up test payer keypair..."
PAYER_KEY="/tmp/solana-test-payer.json"
solana-keygen new -o "$PAYER_KEY" --no-bip39-passphrase --force --silent 2>&1 | tail -1
solana config set --keypair "$PAYER_KEY" --url http://localhost:8899 2>&1 | tail -1

echo "Airdropping SOL for test..."
solana airdrop 100 --url http://localhost:8899 2>&1 | tail -1
sleep 1

echo "Running Solana local integration tests..."
RUST_LOG=info cargo test --test solana_local -- --nocapture 2>&1

echo ""
echo "=== Local Solana tests passed ==="
