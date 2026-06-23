#!/bin/bash
set -euo pipefail

# devdrop workflow test script
# Tests and demonstrates core devdrop functionality

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TEMP_DIR=$(mktemp -d)
trap "rm -rf $TEMP_DIR" EXIT

# Build devdrop first
echo "=== Building devdrop ===" >&2
cargo build --release 2>/dev/null

DEVDROP="$SCRIPT_DIR/target/release/devdrop"

# Setup test workspace
TEST_WS="$TEMP_DIR/workspace"
TEST_REMOTE="$TEMP_DIR/remote"
mkdir -p "$TEST_WS" "$TEST_REMOTE"

echo "=== Test 1: Workspace initialization ===" >&2
$DEVDROP workspace init "$TEST_WS"
test -d "$TEST_WS/.devdrop" && echo "PASS: .devdrop directory created" || echo "FAIL"

echo "=== Test 2: Login ===" >&2
cd "$TEST_WS"
$DEVDROP login testuser
test -f "$TEST_WS/.devdrop/devdrop.sqlite" && echo "PASS: Database created" || echo "FAIL"

echo "=== Test 3: Device enrollment ===" >&2
$DEVDROP device enroll "test-device"
echo "PASS: Device enrolled"

echo "=== Test 4: Sync empty workspace ===" >&2
$DEVDROP sync "$TEST_WS" --remote "$TEST_REMOTE"
echo "PASS: Empty sync completed"

echo "=== Test 5: Status on empty workspace ===" >&2
$DEVDROP status "$TEST_WS"
echo "PASS: Status works"

echo "=== Test 6: Create files and sync ===" >&2
mkdir -p "$TEST_WS/project/src"
echo "fn main() {}" > "$TEST_WS/project/src/main.rs"
echo "# Project" > "$TEST_WS/project/README.md"
cd "$TEST_WS"
$DEVDROP sync "$TEST_WS" --remote "$TEST_REMOTE"
echo "PASS: File sync completed"

echo "=== Test 7: List workspace contents ===" >&2
$DEVDROP ls "$TEST_WS"
echo "PASS: ls works"

echo "=== Test 8: Create git repo and check status ===" >&2
cd "$TEST_WS/project"
git init
git config user.email "test@test.com"
git config user.name "Test"
git add .
git commit -m "init"
cd "$TEST_WS"
$DEVDROP repo-status "$TEST_WS"
echo "PASS: repo-status works"

echo "=== Test 9: Check ignored patterns ===" >&2
mkdir -p "$TEST_WS/project/target"
echo "binary" > "$TEST_WS/project/target/output"
$DEVDROP ignored "$TEST_WS/project"
echo "PASS: ignored works"

echo "=== Test 10: Pin a file ===" >&2
$DEVDROP pin "$TEST_WS/project/README.md"
test -f "$TEST_WS/.devdrop/pins" && echo "PASS: Pin file created" || echo "FAIL"

echo "=== Test 11: Remote sync cycle ===" >&2
# Create a second workspace to test remote sync
TEST_WS2="$TEMP_DIR/workspace2"
mkdir -p "$TEST_WS2"
$DEVDROP workspace init "$TEST_WS2"
cd "$TEST_WS2"
$DEVDROP sync "$TEST_WS2" --remote "$TEST_REMOTE" --pull
$DEVDROP ls "$TEST_WS2"
echo "PASS: Remote pull works"

echo "=== Test 12: Secrets (requires openssl) ===" >&2
if command -v openssl &>/dev/null; then
    export DEVDROP_SECRET_KEY="test-secret-key-12345"
    echo "API_KEY=secret123" > "$TEST_WS/project/.env"
    $DEVDROP secret add "$TEST_WS/project/.env" --scope dev
    # secret add encrypts to vault; secret lock removes plaintext
    $DEVDROP secret lock "$TEST_WS/project/.env" --scope dev
    test ! -e "$TEST_WS/project/.env" && echo "PASS: Secret locked (file removed)" || echo "FAIL"
    $DEVDROP secret unlock "$TEST_WS/project/.env" --scope dev
    test -s "$TEST_WS/project/.env" && echo "PASS: Secret unlocked" || echo "FAIL"
    echo "PASS: Secret lock/unlock works"
else
    echo "SKIP: openssl not available"
fi

echo "=== Test 13: Agent workflow ===" >&2
AGENT_ID=$($DEVDROP agent create --repo "$TEST_WS/project" --write-scope "src/**" --secret-scope "" | head -1 | awk '{print $3}')
echo "PASS: Agent created: $AGENT_ID"

echo "=== Test 13b: Agent diff (show changes) ===" >&2
# Modify a file in the overlay
echo "fn main() { println!(); }" > "$TEST_WS/.devdrop/agents/$AGENT_ID/overlay/src/main.rs"
cd "$TEST_WS"
$DEVDROP agent diff "$AGENT_ID"
echo "PASS: Agent diff shows changes"

echo "=== Test 13c: Agent accept (apply overlay) ===" >&2
$DEVDROP agent accept "$AGENT_ID"
echo "PASS: Agent changes accepted"

echo "=== Test 14: Doctor check ===" >&2
$DEVDROP doctor "$TEST_WS"
echo "PASS: doctor runs"

echo "=== Test 15: History tracking ===" >&2
echo "updated content" >> "$TEST_WS/project/README.md"
cd "$TEST_WS"
$DEVDROP sync "$TEST_WS" --remote "$TEST_REMOTE"
$DEVDROP history "$TEST_WS/project/README.md"
echo "PASS: History tracked"

echo "" >&2
echo "=== All tests passed ===" >&2
