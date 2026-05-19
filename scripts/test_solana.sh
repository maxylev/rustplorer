#!/usr/bin/env bash
set -euo pipefail

echo "=== rustplorer local Solana integration test ==="

if ! command -v solana-test-validator &>/dev/null; then
    echo "Error: solana-test-validator not found."
    echo "Install: sh -c \"\$(curl -sSfL https://release.anza.xyz/stable/install)\""
    exit 1
fi

cleanup() {
    if [ -n "$VALIDATOR_PID" ]; then
        echo "Stopping solana-test-validator (PID: $VALIDATOR_PID)..."
        kill "$VALIDATOR_PID" 2>/dev/null || true
        wait "$VALIDATOR_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT

echo "Starting solana-test-validator..."
solana-test-validator --reset --quiet &
VALIDATOR_PID=$!

echo "Waiting for validator to be ready..."
for i in $(seq 1 60); do
    if curl -s -X POST "http://localhost:8899" \
        -H "Content-Type: application/json" \
        -d '{"jsonrpc":"2.0","id":1,"method":"getHealth"}' | grep -q "ok" 2>/dev/null; then
        echo "Validator ready after ${i}s"
        break
    fi
    sleep 1
done

echo "Running Solana local integration tests..."
cargo test --test solana_local -- --nocapture 2>&1

echo ""
echo "=== Local Solana tests passed ==="
