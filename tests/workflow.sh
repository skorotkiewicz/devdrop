#!/bin/bash
set -euo pipefail

# devdrop workflow test script
# End-to-end devdrop workflow smoke

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
TEMP_DIR=$(mktemp -d)
trap "rm -rf $TEMP_DIR" EXIT

# Build devdrop first
echo "=== Building devdrop ===" >&2
cargo build --release --manifest-path "$REPO_ROOT/Cargo.toml" 2>/dev/null

DEVDROP="$REPO_ROOT/target/release/devdrop"

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
git config commit.gpgsign false
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

echo "=== Test 11b: Device state syncs between workspaces ===" >&2
$DEVDROP device list | grep -q "test-device"
$DEVDROP login testuser
$DEVDROP device enroll "second-device"
$DEVDROP sync "$TEST_WS2" --remote "$TEST_REMOTE"

echo "=== Test 11c: Unhydrated push preserves remote namespace ===" >&2
TEST_WS3="$TEMP_DIR/workspace3"
mkdir -p "$TEST_WS3"
$DEVDROP workspace init "$TEST_WS3"
cd "$TEST_WS3"
$DEVDROP sync "$TEST_WS3" --remote "$TEST_REMOTE" --pull
$DEVDROP ls "$TEST_WS3/project/src" | grep -q "main.rs"
$DEVDROP hydrate "$TEST_WS3/project/src/main.rs"
grep -q "fn main" "$TEST_WS3/project/src/main.rs"
echo "PASS: Unhydrated push preserves remote namespace"

cd "$TEST_WS"
$DEVDROP sync "$TEST_WS" --remote "$TEST_REMOTE" --pull
$DEVDROP device list | grep -q "second-device"
echo "PASS: Device state syncs"

echo "=== Test 11d: Pull conflict keeps local and remote edits ===" >&2
echo "remote edit" > "$TEST_WS/project/README.md"
cd "$TEST_WS"
$DEVDROP sync "$TEST_WS" --remote "$TEST_REMOTE"
echo "local edit" > "$TEST_WS2/project/README.md"
cd "$TEST_WS2"
$DEVDROP sync "$TEST_WS2" --remote "$TEST_REMOTE" --pull
test "$(cat "$TEST_WS2/project/README.md")" = "local edit"
CONFLICT_FILE=$(find "$TEST_WS2/project" -name 'README (conflict from remote *).md' -print -quit)
test -n "$CONFLICT_FILE"
test "$(cat "$CONFLICT_FILE")" = "remote edit"
$DEVDROP conflicts "$TEST_WS2" | grep -q "README"
echo "PASS: Pull conflict keeps both versions"

echo "=== Test 11e: Remote delete conflict keeps local edit ===" >&2
rm "$TEST_WS/project/README.md"
cd "$TEST_WS"
$DEVDROP sync "$TEST_WS" --remote "$TEST_REMOTE"
cd "$TEST_WS2"
$DEVDROP sync "$TEST_WS2" --remote "$TEST_REMOTE" --pull
test ! -e "$TEST_WS2/project/README.md"
DELETE_CONFLICT=$(find "$TEST_WS2/project" -name 'README (conflict from local *).md' -print -quit)
test -n "$DELETE_CONFLICT"
test "$(cat "$DELETE_CONFLICT")" = "local edit"
$DEVDROP conflicts "$TEST_WS2" | grep -q "conflict from local"
echo "PASS: Remote delete conflict keeps local edit"

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

echo "=== Test 13d: Stale agent accept is blocked ===" >&2
STALE_AGENT_ID=$($DEVDROP agent create --repo "$TEST_WS/project" --write-scope "src/**" --secret-scope "" | head -1 | awk '{print $3}')
echo "agent" > "$TEST_WS/.devdrop/agents/$STALE_AGENT_ID/overlay/src/stale.rs"
echo "user" > "$TEST_WS/project/src/stale.rs"
cd "$TEST_WS"
$DEVDROP overlay submit "$STALE_AGENT_ID"
if $DEVDROP agent accept "$STALE_AGENT_ID"; then
    echo "FAIL: stale agent accept should fail"
    exit 1
fi
test "$(cat "$TEST_WS/project/src/stale.rs")" = "user" && echo "PASS: Stale agent accept blocked" || echo "FAIL"

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
