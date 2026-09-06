#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

if [ "$(cargo about --version)" != 'cargo-about 0.9.2' ]; then
  echo 'Install cargo-about 0.9.2 with --locked --features cli' >&2
  exit 1
fi
generated="$(mktemp)"
inventory="$(mktemp)"
trap 'rm -f "$generated" "$inventory"' EXIT
# --fail alone still permits generic SPDX text without upstream copyright
# notices. Require a harvested or checksum-verified source for every text.
cargo about generate --locked --fail --manifest-path crates/cli/Cargo.toml \
  -c packaging/release/about.toml --format json -o "$inventory"
python3 - "$inventory" <<'PY'
import json
import sys
data = json.load(open(sys.argv[1]))
if not data['crates'] or not data['licenses']:
    sys.exit('license inventory is empty')
missing = sorted({u['crate']['name'] for text in data['licenses']
                  if not text.get('source_path') or not text['text'].strip()
                  for u in text['used_by']})
if missing:
    sys.exit('upstream license text missing: ' + ', '.join(missing))
PY
cargo about generate --locked --fail --manifest-path crates/cli/Cargo.toml \
  -c packaging/release/about.toml packaging/release/about.hbs -o "$generated"
# Registry license files have mixed line endings; match the repository's
# LF checkout policy without altering the checksum-verified upstream files.
python3 - "$generated" <<'PY'
from pathlib import Path
import sys
path = Path(sys.argv[1])
text = path.read_text()
path.write_text('\n'.join(line.rstrip(' \t') for line in text.split('\n')).rstrip('\n') + '\n')
PY
if [ "${1:-}" = --write ]; then
  cp "$generated" crates/cli/assets/THIRD_PARTY_NOTICES.txt
else
  cmp "$generated" crates/cli/assets/THIRD_PARTY_NOTICES.txt || {
    echo 'Rust notices are stale; run scripts/check-rust-notices.sh --write' >&2
    exit 1
  }
fi
