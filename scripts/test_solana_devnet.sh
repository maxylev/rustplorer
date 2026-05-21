#!/usr/bin/env bash
set -euo pipefail

echo "=== rustplorer Solana Devnet E2E Test ==="
echo ""

need_cmd() {
    if ! command -v "$1" &>/dev/null; then
        echo "Error: $1 not found. $2"
        exit 1
    fi
}

need_cmd solana  "Install Solana CLI: https://docs.solana.com/cli/install-solana-cli"
need_cmd spl-token "Install SPL Token CLI: part of Solana CLI"

# ---------------------------------------------------------------------------
# Test Accounts (pre-funded on devnet)
# ---------------------------------------------------------------------------
BUYER_PK="3DXUJncvBrbpDLFs66o1L88PxNFBca1F3rvxtp6NRvPcFQT7Pu484RpZyaKNQWSUixHSBiaajdHyHN6sdA1nDiAT"
FACI_PK="47ZpJxNrP59UC33SXr5xCYkAsvJucD7cYnKkRYwAeu5aibLozjCXe3oxDRv582MpSRrNwXZH8sCq5mYXkcaZT98d"
BUYER_ADDR="F19y4Ewyw141KspE8HoPjYpbHV8p1x7mjb2DF4XuQXQK"
FACI_ADDR="3VN9g4VZanawKwVgXVDRe99G27yZmqh2Lbd62UpgXQu7"
PYUSDT_MINT="CXk2AMBfi3TwaEL2468s6zP8xq9NxTXjp9gjMgzeUynM"

RPC_URL="https://api.devnet.solana.com"

# ---------------------------------------------------------------------------
# Create keypair files from base58 private keys
# ---------------------------------------------------------------------------
B58DECODE="$HOME/.cargo/bin/b58decode"
if [ ! -f "$B58DECODE" ]; then
    cat > /tmp/b58decode.rs << 'EOF'
use std::fs;

fn b58decode(input: &str) -> Vec<u8> {
    const ALPHA: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    let mut bytes = vec![0u8; input.len() * 11 / 15 + 1];
    for c in input.chars() {
        let carry = ALPHA.iter().position(|&x| x == c as u8).unwrap();
        let mut b = carry;
        for byte in bytes.iter_mut().rev() { b += 58 * *byte as usize; *byte = (b % 256) as u8; b /= 256; }
    }
    while bytes.first() == Some(&0) { bytes.remove(0); }
    while bytes.len() < 64 { bytes.insert(0, 0); }
    bytes
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let bytes = b58decode(&args[1]);
    let json = format!("[{}]", bytes.iter().map(|b| b.to_string()).collect::<Vec<_>>().join(","));
    fs::write(&args[2], json).unwrap();
}
EOF
    rustc /tmp/b58decode.rs -o "$B58DECODE"
fi

BUYER_KEYFILE="/tmp/solana-test-buyer-key.json"
FACI_KEYFILE="/tmp/solana-test-faci-key.json"
CONFIG_FILE="/tmp/solana-devnet-config.toml"
ADDR_FILE="/tmp/solana-devnet-addresses.txt"

"$B58DECODE" "$BUYER_PK" "$BUYER_KEYFILE"
"$B58DECODE" "$FACI_PK" "$FACI_KEYFILE"

echo "Keypair files created."

# ---------------------------------------------------------------------------
# Show initial balances
# ---------------------------------------------------------------------------
echo ""
echo "--- Initial Balances ---"
echo "Buyer ($BUYER_ADDR):   $(solana balance --keypair $BUYER_KEYFILE --url devnet 2>&1)"
echo "Facilitator ($FACI_ADDR): $(solana balance --keypair $FACI_KEYFILE --url devnet 2>&1)"
echo "Buyer PYUSDT:    $(spl-token balance --owner $BUYER_ADDR $PYUSDT_MINT --url devnet 2>&1)"
echo "Facilitator PYUSDT: $(spl-token balance --owner $FACI_ADDR $PYUSDT_MINT --url devnet 2>&1)"

# ---------------------------------------------------------------------------
# Grab current slot BEFORE transfers
# ---------------------------------------------------------------------------
START_SLOT=$(curl -s -X POST "$RPC_URL" \
    -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","id":1,"method":"getSlot"}' | jq -r '.result')
echo ""
echo "Start slot: $START_SLOT"

# ---------------------------------------------------------------------------
# Execute transfers on devnet
# ---------------------------------------------------------------------------
echo ""
echo "--- Executing Transfers ---"

echo "Sending 0.003 SOL from buyer to facilitator..."
SOL_TX=$(solana transfer --keypair "$BUYER_KEYFILE" \
    --url devnet --allow-unfunded-recipient \
    "$FACI_ADDR" 0.003 2>&1)
SOL_SIG=$(echo "$SOL_TX" | grep -oE '[1-9A-HJ-NP-Za-km-z]{44,88}' | head -1)
echo "  SOL tx signature: $SOL_SIG"
echo "  OK"

echo "Sending 1.5 PYUSDT from buyer to facilitator..."
SPL_TX=$(spl-token transfer --owner "$BUYER_KEYFILE" \
    --url devnet --fund-recipient \
    "$PYUSDT_MINT" 1.5 "$FACI_ADDR" 2>&1)
SPL_SIG=$(echo "$SPL_TX" | grep -oE '[1-9A-HJ-NP-Za-km-z]{44,88}' | head -1)
echo "  SPL tx signature: $SPL_SIG"
echo "  OK"

echo "Waiting for devnet confirmation..."
sleep 15

# ---------------------------------------------------------------------------
# Grab ending slot
# ---------------------------------------------------------------------------
END_SLOT=$(curl -s -X POST "$RPC_URL" \
    -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","id":1,"method":"getSlot"}' | jq -r '.result')
echo ""
echo "End slot: $END_SLOT"
echo "Scan range: $START_SLOT - $END_SLOT"

# ---------------------------------------------------------------------------
# Create test config with Solana devnet
# ---------------------------------------------------------------------------
cat > "$CONFIG_FILE" << TOML
[chains.solana]
caip2 = "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp"
start_block = $START_SLOT
end_block = $END_SLOT
rpc = [
    "https://api.devnet.solana.com",
]

  [chains.solana.rpc_options]
  max_concurrent = 1
  delay_ms = 200

  [chains.solana.assets.SOL_NATIVE]
  contract = "native"
  decimals = 9

  [chains.solana.assets.PYUSDT]
  contract = "$PYUSDT_MINT"
  decimals = 6
TOML

echo "Facilitator address to track: $FACI_ADDR"
echo "$FACI_ADDR" > "$ADDR_FILE"

# ---------------------------------------------------------------------------
# Build and run rustplorer
# ---------------------------------------------------------------------------
echo ""
echo "--- Building rustplorer ---"
cargo build -q 2>&1
echo "Build OK."

echo ""
echo "--- Running rustplorer scan ---"
RESULT=$(cargo run -q -- --config "$CONFIG_FILE" --addresses "$ADDR_FILE" --verbose 2>&1)
echo "$RESULT"

# ---------------------------------------------------------------------------
# Verify results
# ---------------------------------------------------------------------------
echo ""
echo "--- Verifying Deposits ---"

# Scanner uses "Native" as the asset name for native SOL transfers
SOL_DEPOSIT=$(echo "$RESULT" | jq -r '.[] | select(.asset == "Native" and .to_address == "'$FACI_ADDR'" and .amount_clean == "0.003") | "\(.amount_clean) SOL"' 2>/dev/null)
SPL_DEPOSIT=$(echo "$RESULT" | jq -r '.[] | select(.asset == "'$PYUSDT_MINT'" and .to_address == "'$FACI_ADDR'" and .amount_clean == "1.5") | "\(.amount_clean) PYUSDT"' 2>/dev/null)

PASS=0
FAIL=0

echo "Raw scan result:"
echo "$RESULT" | jq -r '.[] | "  chain=\(.chain) asset=\(.asset) to=\(.to_address) amount=\(.amount_clean) block=\(.block_number)"' 2>/dev/null

if [ -n "$SOL_DEPOSIT" ]; then
    echo "  PASS: SOL native deposit detected -> $SOL_DEPOSIT"
    PASS=$((PASS + 1))
else
    echo "  FAIL: No SOL native deposit for tx $SOL_SIG"
    FAIL=$((FAIL + 1))
fi

if [ -n "$SPL_DEPOSIT" ]; then
    echo "  PASS: PYUSDT SPL deposit detected -> $SPL_DEPOSIT"
    PASS=$((PASS + 1))
else
    echo "  FAIL: No PYUSDT deposit for tx $SPL_SIG"
    FAIL=$((FAIL + 1))
fi

# ---------------------------------------------------------------------------
# Show final balances
# ---------------------------------------------------------------------------
echo ""
echo "--- Final Balances ---"
echo "Buyer ($BUYER_ADDR):   $(solana balance --keypair $BUYER_KEYFILE --url devnet 2>&1)"
echo "Facilitator ($FACI_ADDR): $(solana balance --keypair $FACI_KEYFILE --url devnet 2>&1)"
echo "Buyer PYUSDT:    $(spl-token balance --owner $BUYER_ADDR $PYUSDT_MINT --url devnet 2>&1)"
echo "Facilitator PYUSDT: $(spl-token balance --owner $FACI_ADDR $PYUSDT_MINT --url devnet 2>&1)"

echo ""
echo "=== Results: $PASS PASS, $FAIL FAIL ==="
if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
echo "=== All Solana devnet e2e tests passed ==="
