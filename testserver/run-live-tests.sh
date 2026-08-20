#!/usr/bin/env bash
# Run the live SSH tests against a throwaway container.
#
# HOME is redirected at a scratch directory on purpose: accepting the test
# server's host key writes to $HOME/.ssh/known_hosts, and your real one should
# not collect entries for a container that is about to be deleted.
set -euo pipefail

cd "$(dirname "$0")/.."

CONTAINER=sshman-test
PORT=${PORT:-2222}

if ! docker image inspect "$CONTAINER" >/dev/null 2>&1; then
    echo "==> building the test image"
    docker build -t "$CONTAINER" testserver
fi

if ! docker ps --filter "name=^${CONTAINER}$" --format '{{.Names}}' | grep -q .; then
    echo "==> starting the test server on port $PORT"
    docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
    docker run -d --name "$CONTAINER" -p "$PORT:22" "$CONTAINER" >/dev/null
    sleep 3
fi

# Build the test binary and pull its path out of cargo's JSON output, so we can
# run it with a modified environment without also affecting cargo itself.
BIN=$(cargo test --no-run --message-format=json 2>/dev/null | python3 -c '
import sys, json
for line in sys.stdin:
    try:
        d = json.loads(line)
    except ValueError:
        continue
    if (d.get("reason") == "compiler-artifact"
            and d.get("target", {}).get("name") == "sshman"
            and d.get("profile", {}).get("test")):
        print(d["executable"])
')

SCRATCH=$(mktemp -d)
trap 'rm -rf "$SCRATCH"' EXIT

echo "==> running live tests"
HOME="$SCRATCH" \
SSHMAN_TEST_HOST=localhost \
SSHMAN_TEST_PORT="$PORT" \
SSHMAN_TEST_USER=tester \
SSHMAN_TEST_PASS=testpass \
SSHMAN_TEST_SUDO_PASS=testpass \
SSHMAN_TEST_CONTAINER="$CONTAINER" \
    "$BIN" --ignored --test-threads=1 "$@"

echo
echo "Stop the server with: docker rm -f $CONTAINER"
