---
title: Detect drift between the actions allowlist file and the GitHub selected-actions setting
priority: 3
schedule: once
---

## Goal

When `.github/actions-allowlist.json` changes, the repository's selected-actions setting on GitHub has to be re-applied by hand. Nothing notices when that step is missed. Make the mismatch visible before or soon after a merge, without giving CI the power to change the setting.

## Context

The allowlist exists in two places:

- `.github/actions-allowlist.json`, checked in. `scripts/check-actions-allowlist.py` runs in the `supply-chain` job of `.github/workflows/ci.yml` and verifies that every workflow `uses:` reference is an exact entry in the file. It is offline by design.
- The repository's Actions setting (`allowed_actions: selected` with `patterns_allowed`), held by GitHub. This is the one GitHub enforces.

`docs/public-readiness.md` says the operator must re-apply the file to the setting after it changes. `.github/dependabot.yml` says an Actions bump must add the new commit to the file in the same PR. Neither step is checked against the other.

What happens when the setting lags the file: GitHub refuses to start any workflow that references a commit the setting does not list. The run ends as `startup_failure` with "This run likely failed because of a workflow file issue". A startup failure cannot be rerun.

Seen on 2026-11-27. PR #243 moved `github/codeql-action` to v4.39.4 and `anchore/sbom-action` to v0.24.4 and updated the file. The setting still listed the old commits.

- `codeql` failed at startup on the PR and then on main, so code scanning did not run on main for several hours. `codeql` is not a required check, so the PR merged green.
- `release.yml` referenced the new `sbom-action` commit and would have failed at startup on the next release.
- The setting also still listed the previous `softprops/action-gh-release` commit from an allowlist change made earlier the same week. That mismatch had gone unnoticed for days because `release.yml` only runs on a release.

The operator approved re-applying the setting on 2026-11-27 and it was done through the GitHub API. The setting matched the file at that point, and a `codeql` run on main then passed.

A workflow that fails at startup cannot report on itself. A check that lives only inside an affected workflow would not have caught this.

## D-X operator decision

Where the comparison runs.

- D-X1: a local script, for example `scripts/check-actions-setting.py`, that reads the setting with the operator's own `gh` credentials and compares it with the file. Documented as a step in the Actions-bump routine in `docs/public-readiness.md`. No new credential in CI. It relies on someone running it.
- D-X2: D-X1, plus a CI job that runs the same comparison on pull requests that touch `.github/actions-allowlist.json` and on a schedule. This needs a token that can read the repository's Actions settings. The default `GITHUB_TOKEN` is not expected to have that permission (to be confirmed during scoping), so it means a new read-only repository secret and a decision about which jobs may see it.
- D-X3: documentation only. Add the re-apply step and the `startup_failure` symptom to the Dependabot routine and to `docs/public-readiness.md`.

A related, smaller choice that can be made on its own: make `codeql` a required check, so a startup failure blocks the merge. It would have stopped #243. It does not cover `release.yml`.

## Scoping answers

- The check only reads. Applying the setting stays an operator action. No job and no script in this ticket writes to the repository's Actions settings.
- Compare all three fields: `patterns_allowed` as a set, `github_owned_allowed` and `verified_allowed`. Report entries missing from the setting and entries the setting has that the file does not.
- Keep `scripts/check-actions-allowlist.py` offline and deterministic. The network comparison is a separate script or a separate mode, and its absence of credentials must not fail the existing `supply-chain` job.
- Under D-X2, a pull request from a fork gets no secret. The job must skip with a clear message there and must not fail open silently on main.
- Any new workflow or job follows the existing rules: actions pinned by full commit SHA and listed in the file, least-privilege `permissions`.
- A file change that is ahead of the setting is the normal state of an Actions-bump PR before the operator re-applies it. The check should say which side is behind, so the operator knows whether to apply the setting or fix the file.

## Acceptance hints

- A self-test in the style of `check-actions-allowlist.py --self-test` that feeds the comparison a file and a setting payload and covers: identical, an entry missing from the setting, an extra entry in the setting, and a differing `github_owned_allowed` or `verified_allowed`.
- The comparison exits nonzero on any mismatch and prints each differing entry.
- `docs/public-readiness.md` names the check and the `startup_failure` symptom beside the existing re-apply instruction.
- The existing `supply-chain` job still passes with no network access to the settings API.
