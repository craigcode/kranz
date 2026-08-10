#!/bin/sh
# order-trigger-event.sh — prove the dispatch order is event-driven.
set -u

ORDER=$(CDPATH= cd -- "$(dirname -- "$0")/../orders" && pwd)/kranz-dispatch.toml

if ! grep -qE '^trigger\s*=\s*"event"' "$ORDER"; then
    echo "FAIL: kranz-dispatch.toml is not event-triggered" >&2
    exit 1
fi

if grep -qE '^(cooldown|interval)\s*=' "$ORDER"; then
    echo "FAIL: kranz-dispatch.toml still contains cooldown/interval keys" >&2
    exit 1
fi

if ! grep -qE '^on\s*=\s*"bead\.' "$ORDER"; then
    echo "FAIL: kranz-dispatch.toml has no bead event 'on' key" >&2
    exit 1
fi

# The authoritative shape check: gc lint on the whole pack passes.
REPO_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../../.." && pwd)
if ! gc lint "$REPO_ROOT/packaging/gascity" >/dev/null 2>&1; then
    echo "FAIL: gc lint packaging/gascity" >&2
    exit 1
fi

echo "ORDER-TRIGGER-EVENT: PASS"
