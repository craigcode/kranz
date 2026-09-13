#!/usr/bin/env bash
set -euo pipefail

python3 "$(dirname "$0")/audit-operator-markers.py" history

# Release-only audit. This intentionally scans every reachable ref, not merely
# the release diff: public visibility exposes old blobs and tags too.
if [ "${KRANZ_SKIP_GITLEAKS:-0}" != 1 ]; then
  command -v gitleaks >/dev/null 2>&1 || {
    echo 'public-history audit: gitleaks is required' >&2
    exit 1
  }
  gitleaks git --no-banner --redact --log-opts='--all'
fi

printf 'public-history audit: ok\n'
