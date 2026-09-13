#!/usr/bin/env bash
set -euo pipefail

# Operator vocabulary is supplied privately; never encode it in public source.
python3 "$(dirname "$0")/audit-operator-markers.py" tree
printf 'public-tree audit: ok\n'
