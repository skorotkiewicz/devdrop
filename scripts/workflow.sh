#!/usr/bin/env -S SHLVL=0 bash
set -euo pipefail

# End-to-end devdrop workflow smoke test.
# SSH remote smoke target.
# DEVDROP_TEST_SSH_REMOTE="${DEVDROP_TEST_SSH_REMOTE:-ssh://mod@ml/home/mod/code/devdrop-workflow-smoke}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
TEMP_DIR=$(mktemp -d)
trap "rm -rf $TEMP_DIR" EXIT

echo "=== Building devdrop ===" >&2
cargo build --release --manifest-path "$REPO_ROOT/Cargo.toml" >/dev/null

DEVDROP="$REPO_ROOT/target/release/devdrop"
TEST_WS="$TEMP_DIR/workspace"
TEST_WS2="$TEMP_DIR/workspace2"
TEST_WS3="$TEMP_DIR/workspace3"
TEST_REMOTE="$TEMP_DIR/remote"
mkdir -p "$TEST_WS" "$TEST_WS2" "$TEST_WS3" "$TEST_REMOTE"

echo "=== Test 1: init --remote writes .devdrop.toml ===" >&2
"$DEVDROP" init "$TEST_WS" --remote "$TEST_REMOTE"
test -d "$TEST_WS/.devdrop"
test -f "$TEST_WS/.devdrop.toml"
grep -q "$TEST_REMOTE" "$TEST_WS/.devdrop.toml"
(cd "$TEST_WS" && "$DEVDROP" remote ls | grep -q "$TEST_REMOTE")
(cd "$TEST_WS" && test "$("$DEVDROP" config get remote.url)" = "$TEST_REMOTE")
echo "PASS: init --remote creates workspace config"

echo "=== Test 2: plain sync uses configured remote ===" >&2
mkdir -p "$TEST_WS/project/src"
printf 'fn main() {}\n' > "$TEST_WS/project/src/main.rs"
printf '# Project\n' > "$TEST_WS/project/README.md"
(cd "$TEST_WS" && "$DEVDROP" sync)
test -f "$TEST_REMOTE/manifests/latest.tsv"
test -d "$TEST_REMOTE/objects"
echo "PASS: sync pushed manifest and objects"

echo "=== Test 3: second workspace plain sync pulls and hydrates ===" >&2
"$DEVDROP" init "$TEST_WS2" --remote "$TEST_REMOTE"
(cd "$TEST_WS2" && "$DEVDROP" sync)
grep -q "fn main" "$TEST_WS2/project/src/main.rs"
grep -q "# Project" "$TEST_WS2/project/README.md"
echo "PASS: plain sync pulls and hydrates files"

echo "=== Test 4: normal remote edit updates local without conflict ===" >&2
printf 'from workspace2\n' > "$TEST_WS2/project/README.md"
(cd "$TEST_WS2" && "$DEVDROP" sync)
(cd "$TEST_WS" && "$DEVDROP" sync)
test "$(cat "$TEST_WS/project/README.md")" = "from workspace2"
test -z "$(find "$TEST_WS/project" -name '*conflict*' -print -quit)"
echo "PASS: remote edit updates clean local file"

echo "=== Test 5: divergent edits create one conflict sibling ===" >&2
printf 'local divergence\n' > "$TEST_WS/project/README.md"
printf 'remote divergence\n' > "$TEST_WS2/project/README.md"
(cd "$TEST_WS2" && "$DEVDROP" sync)
(cd "$TEST_WS" && "$DEVDROP" sync)
test "$(cat "$TEST_WS/project/README.md")" = "local divergence"
CONFLICT_FILE=$(find "$TEST_WS/project" -name 'README.conflict-remote-*.md' -print -quit)
test -n "$CONFLICT_FILE"
test "$(cat "$CONFLICT_FILE")" = "remote divergence"
"$DEVDROP" conflicts "$TEST_WS" | grep -q "README.conflict-remote"
"$DEVDROP" conflicts resolve "$CONFLICT_FILE" --use conflict
test "$(cat "$TEST_WS/project/README.md")" = "remote divergence"
echo "PASS: divergent edits conflict and resolve"

echo "=== Test 6: remote delete conflict preserves local edit ===" >&2
printf 'common delete file\n' > "$TEST_WS/project/delete-me.txt"
(cd "$TEST_WS" && "$DEVDROP" sync)
(cd "$TEST_WS2" && "$DEVDROP" sync)
printf 'local delete-conflict edit\n' > "$TEST_WS2/project/delete-me.txt"
rm "$TEST_WS/project/delete-me.txt"
(cd "$TEST_WS" && "$DEVDROP" sync)
(cd "$TEST_WS2" && "$DEVDROP" sync)
test ! -e "$TEST_WS2/project/delete-me.txt"
DELETE_CONFLICT=$(find "$TEST_WS2/project" -name 'delete-me.conflict-local-*.txt' -print -quit)
test -n "$DELETE_CONFLICT"
test "$(cat "$DELETE_CONFLICT")" = "local delete-conflict edit"
"$DEVDROP" conflicts resolve "$DELETE_CONFLICT" --use conflict
test "$(cat "$TEST_WS2/project/delete-me.txt")" = "local delete-conflict edit"
echo "PASS: delete conflict preserves and resolves local edit"

echo "=== Test 7: config pins are read from .devdrop.toml ===" >&2
(cd "$TEST_WS" && "$DEVDROP" config set pins project/src)
(cd "$TEST_WS" && test "$("$DEVDROP" config get pins)" = "project/src")
grep -q 'pins = \["project/src"\]' "$TEST_WS/.devdrop.toml"
echo "PASS: config pins round-trip"

echo "=== Test 8: remote add writes configured remote ===" >&2
"$DEVDROP" init "$TEST_WS3"
(cd "$TEST_WS3" && "$DEVDROP" remote add "$TEST_REMOTE")
(cd "$TEST_WS3" && "$DEVDROP" remote ls | grep -q "$TEST_REMOTE")
(cd "$TEST_WS3" && "$DEVDROP" sync)
echo "PASS: remote add makes sync work without --remote"

echo "=== Test 9: secret set works without DEVDROP_SECRET_KEY ===" >&2
(cd "$TEST_WS" && env -u DEVDROP_SECRET_KEY "$DEVDROP" secret set API_KEY=secret123)
(cd "$TEST_WS" && env -u DEVDROP_SECRET_KEY "$DEVDROP" secret list | grep -q API_KEY)
SECRET_VALUE=$(cd "$TEST_WS" && env -u DEVDROP_SECRET_KEY "$DEVDROP" run --repo "$TEST_WS" -- printenv API_KEY)
test "$SECRET_VALUE" = "secret123"
test -f "$TEST_WS/.devdrop/secret.key"
echo "PASS: secret set stores and injects env value"

echo "=== Test 10: edit wraps overlay review ===" >&2
EDIT_OUTPUT=$(cd "$TEST_WS" && printf 'n\n' | EDITOR=true "$DEVDROP" edit "$TEST_WS/project")
grep -q "Editing overlay:" <<<"$EDIT_OUTPUT"
grep -q "Changes rejected" <<<"$EDIT_OUTPUT"
echo "PASS: edit opens overlay and prompts accept/reject"

echo "=== Test 11: repo status and ignored files still work ===" >&2
if command -v git >/dev/null 2>&1; then
    cd "$TEST_WS/project"
    git init >/dev/null
    git config user.email "test@test.com"
    git config user.name "Test"
    git config commit.gpgsign false
    git add .
    git commit -m "init" >/dev/null
    mkdir -p "$TEST_WS/project/target"
    printf 'binary\n' > "$TEST_WS/project/target/output"
    "$DEVDROP" repo-status "$TEST_WS" >/dev/null
    "$DEVDROP" ignored "$TEST_WS/project" | grep -q "target"
    echo "PASS: repo-status and ignored work"
else
    echo "SKIP: git not available"
fi

echo "=== Test 12: history, status, doctor ===" >&2
printf 'updated content\n' >> "$TEST_WS/project/README.md"
(cd "$TEST_WS" && "$DEVDROP" sync)
"$DEVDROP" history "$TEST_WS/project/README.md" | grep -q "fnv1a64"
"$DEVDROP" status "$TEST_WS" >/dev/null
"$DEVDROP" doctor "$TEST_WS" >/dev/null
echo "PASS: history, status, and doctor run"

if [[ -n "${DEVDROP_TEST_SSH_REMOTE:-}" ]]; then
    echo "=== Test 13: SSH remote backend ===" >&2
    SSH_WS="$TEMP_DIR/ssh-workspace"
    mkdir -p "$SSH_WS"
    printf 'ssh smoke\n' > "$SSH_WS/file.txt"
    "$DEVDROP" init "$SSH_WS" --remote "$DEVDROP_TEST_SSH_REMOTE"
    (cd "$SSH_WS" && "$DEVDROP" sync)
    echo "PASS: SSH remote sync completed"
else
    echo "SKIP: set DEVDROP_TEST_SSH_REMOTE=ssh://host/path to test SSH remote"
fi

echo "" >&2
echo "=== All tests passed ===" >&2
