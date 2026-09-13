# Security review log

This index connects review findings to remediation and regression evidence.
A review covers its named revision, surfaces, and method. It is not a claim
that every platform or every later revision has been audited. Some historical
commit IDs belong to the private development lineage; the public history
starts at `05778ae`. The linked reports were retained in that public snapshot.

| Review | Method and scope recorded by the report | Findings, fixes, and limits |
|---|---|---|
| [September 1 adversarial audit](reviews/adversarial-audit-2026-09-01.md) | Seven parallel adversarial reviewers; static tracing across server, sandbox, execution, backends, Slack, state, CLI and supply chain. The report distinguishes confirmed paths from plausible OS behavior. | Includes September 3 remediation, named regression tests, follow-up reviews, and residuals. Historical severity counts describe the audited revision, not current open issues. |
| [September 5 release candidate](reviews/2026-09-05-release-candidate.md) | Source review plus local and live execution evidence for release hardening. | Documents webhook authorization, Git and sandbox boundaries, process cleanup, and integrity fixes; links the later acceptance receipt rather than treating early coverage gaps as current. |
| [September 11 remediation](reviews/2026-09-11-audit-remediation.md) | Six findings covering review bypass, Git configuration, approval identity, imports, ticket writes, and artifact reads; local macOS regression runs. | Finding-to-change-to-test table, full gate results, and limitations including Git preflight races and accepted Tauri advisories. Platform claims are scoped to recorded runs. |
| [v0.2.1 public-review response](reviews/2026-09-13-server-security-patch.md) | Response to a supplied static review of public `05778ae`; that reviewer did not build or deeply audit orchestration, sandbox, or Windows AppContainer. | Streamed POST media types, route-local hook authentication, and end-to-end read-token exchange. Tests first reproduced the old behavior. |

Follow individual reports for exact test names, evidence, and disposition.
Reviewer identity or independence is not inferred where a report does not
record it. Report new vulnerabilities through [SECURITY.md](../SECURITY.md).
