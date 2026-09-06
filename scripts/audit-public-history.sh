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

markers=(
  '/Users/'craig'martin'
  'craigmartin8008''@''gmail.com'
  'craig''@''craigmartin.com'
  'everything-''evenhub'
)

failed=0
for marker in "${markers[@]}"; do
  matches="$(git log --all --format='%H %cs %s' -G"$marker" -- .)"
  if [ -n "$matches" ]; then
    printf 'public-history audit: marker remains in reachable history: %s\n%s\n' "$marker" "$matches" >&2
    failed=1
  fi

  message_matches="$(git log --all --format='%H%x09%B' | grep -F "$marker" || true)"
  if [ -n "$message_matches" ]; then
    printf 'public-history audit: marker remains in commit messages: %s\n%s\n' \
      "$marker" "$message_matches" >&2
    failed=1
  fi
done

# `git log -G` searches patches, not author/committer headers. Keep identity
# metadata under the same fail-closed privacy gate.
identity_markers=(
  'craig''@''craigmartin.com'
)
for marker in "${identity_markers[@]}"; do
  matches="$(git log --all --format='%ae%n%ce' | sort -u | grep -F "$marker" || true)"
  if [ -n "$matches" ]; then
    printf 'public-history audit: marker remains in commit identity metadata: %s\n%s\n' \
      "$marker" "$matches" >&2
    failed=1
  fi
done

if [ "$failed" -ne 0 ]; then
  cat >&2 <<'EOF'
Do not make the repository public or cut a release. Classify the matched blobs,
rewrite history if removal is required, force-update private refs deliberately,
then rerun this audit from a fresh clone.
EOF
  exit 1
fi

printf 'public-history audit: ok\n'
