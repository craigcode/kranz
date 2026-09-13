#!/bin/sh
# order-trigger-event.sh — prove the dispatch orders are event-driven with a
# cooldown backstop.
set -u

ORDERS=$(CDPATH= cd -- "$(dirname -- "$0")/../orders" && pwd)
DISPATCH="$ORDERS/kranz-dispatch.toml"
BACKSTOP="$ORDERS/kranz-dispatch-backstop.toml"

# --- primary order: event on bead.created, no polling keys ------------------
if ! grep -qE '^trigger\s*=\s*"event"' "$DISPATCH"; then
    echo "FAIL: kranz-dispatch.toml is not event-triggered" >&2
    exit 1
fi

# Exactly bead.created — a bead.* wildcard would silently accept a typo that
# never fires (gc lint does not validate event names; bead.bogus lints ok).
if ! grep -qE '^on\s*=\s*"bead\.created"' "$DISPATCH"; then
    echo "FAIL: kranz-dispatch.toml 'on' is not exactly bead.created" >&2
    exit 1
fi

if grep -qE '^(cooldown|interval)\s*=' "$DISPATCH"; then
    echo "FAIL: kranz-dispatch.toml still contains cooldown/interval keys" >&2
    exit 1
fi

# --- backstop order: slow cooldown reclaim + re-ready drain -----------------
# The reclaim sweep and a re-readied (bead.updated) bead are only picked up
# when kranz-dispatch is invoked; the event order alone starves both in a
# quiet city. The backstop must exist, be cooldown-triggered, and carry an
# interval.
if [ ! -f "$BACKSTOP" ]; then
    echo "FAIL: kranz-dispatch-backstop.toml is missing (reclaim/re-ready starve without it)" >&2
    exit 1
fi
if ! grep -qE '^trigger\s*=\s*"cooldown"' "$BACKSTOP"; then
    echo "FAIL: kranz-dispatch-backstop.toml is not cooldown-triggered" >&2
    exit 1
fi
if ! grep -qE '^interval\s*=\s*"[0-9]+[smh]"' "$BACKSTOP"; then
    echo "FAIL: kranz-dispatch-backstop.toml has no interval" >&2
    exit 1
fi

# The authoritative shape check: gc lint on the whole pack passes.
REPO_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../../.." && pwd)
if ! gc lint "$REPO_ROOT/packaging/gascity" >/dev/null 2>&1; then
    echo "FAIL: gc lint packaging/gascity" >&2
    exit 1
fi

echo "ORDER-TRIGGER-EVENT: PASS"
