#!/usr/bin/env bash
set -euo pipefail

# Release-only audit. This intentionally scans every reachable ref, not merely
# the release diff: public visibility exposes old blobs and tags too.
if [ "${KRANZ_SKIP_GITLEAKS:-0}" != 1 ]; then
  command -v gitleaks >/dev/null 2>&1 || {
    echo 'public-history audit: gitleaks is required' >&2
    exit 1
  }
  gitleaks git --no-banner --redact --log-opts='--all'
fi

markers=()
marker_count=0
marker_file="${KRANZ_PUBLIC_AUDIT_MARKERS_FILE:-}"
require_markers="${KRANZ_REQUIRE_OPERATOR_MARKERS:-0}"
if [ -n "$marker_file" ]; then
  if [ ! -f "$marker_file" ]; then
    printf 'public-history audit: marker file is not readable: %s\n' "$marker_file" >&2
    exit 1
  fi
  while IFS= read -r marker || [ -n "$marker" ]; do
    marker="${marker%$'\r'}"
    if [ -n "$marker" ]; then
      markers+=("$marker")
      marker_count=$((marker_count + 1))
    fi
  done < "$marker_file"
fi
if [ "$require_markers" = 1 ] && [ "$marker_count" -eq 0 ]; then
  echo 'public-history audit: release requires a non-empty external operator-marker file' >&2
  exit 1
fi

failed=0
if [ "$marker_count" -gt 0 ]; then
  for marker in "${markers[@]}"; do
    matches="$(git log --all --format='%H' -S"$marker" -- .)"
    if [ -n "$matches" ]; then
      echo 'public-history audit: external operator marker remains in reachable history' >&2
      failed=1
    fi

    message_matches="$(git log --all --format='%H%x09%B' | grep -F -m 1 -- "$marker" || true)"
    if [ -n "$message_matches" ]; then
      echo 'public-history audit: external operator marker remains in commit messages' >&2
      failed=1
    fi

    # Patch searches do not inspect author/committer headers. Applying every
    # external marker here is harmless and keeps names and email identities
    # under the same fail-closed gate without identifying which markers are
    # identity fragments.
    matches="$(git log --all --format='%an%n%ae%n%cn%n%ce' | sort -u | grep -F -m 1 -- "$marker" || true)"
    if [ -n "$matches" ]; then
      echo 'public-history audit: external operator marker remains in commit identity metadata' >&2
      failed=1
    fi

    # Commit walking does not cover ref names or annotated-tag metadata.
    # GitHub exposes both after publication, so scan them without printing
    # the marker or the matching record.
    ref_matches="$(git for-each-ref \
      --format='%(refname)%0a%(taggername)%0a%(taggeremail)%0a%(contents)' \
      refs/heads refs/remotes refs/tags | grep -F -m 1 -- "$marker" || true)"
    if [ -n "$ref_matches" ]; then
      echo 'public-history audit: external operator marker remains in ref or tag metadata' >&2
      failed=1
    fi
  done
fi

if [ "$failed" -ne 0 ]; then
  cat >&2 <<'EOF'
Do not make the repository public or cut a release. Classify the matched blobs,
rewrite history if removal is required, force-update private refs deliberately,
then rerun this audit from a fresh clone.
EOF
  exit 1
fi

printf 'public-history audit: ok (%s external operator marker(s))\n' "$marker_count"
