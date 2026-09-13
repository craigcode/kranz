#!/bin/sh
# Stub-safe Gas City pack gates for CI.
#
# Requires `gc` on PATH (CI installs a pinned release). Does not create a
# city, does not talk to a live `bd`, and does not run
# kranz-dispatch-roundtrip.sh (that fixture needs a real beads store).
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$ROOT"

if ! command -v gc >/dev/null 2>&1; then
    echo "ci-gascity-pack: gc is not on PATH" >&2
    exit 1
fi

echo "==> gc lint packaging/gascity"
gc lint packaging/gascity

for t in \
    packaging/gascity/test/order-trigger-event.sh \
    packaging/gascity/test/kranz-run-bead-events.sh \
    packaging/gascity/test/kranz-native-queue-selftest.sh \
    packaging/gascity/test/check-bridge-hygiene.sh \
    packaging/gascity/test/check-bridge-hygiene-selftest.sh \
    packaging/gascity/test/kranz-header-liveness-selftest.sh \
    packaging/gascity/test/kranz-header-lease-dir-selftest.sh
do
    echo "==> $t"
    bash "$t"
done

echo "==> packaging/gascity/test/check-bridge-hygiene.sh --status-map"
bash packaging/gascity/test/check-bridge-hygiene.sh --status-map

echo "ci-gascity-pack: ok"
