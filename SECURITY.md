# Security policy

Kranz executes AI-agent CLIs and repository-provided commands. Bugs at its
process, filesystem, credential, authorization, or network boundaries can have
serious consequences. Please report suspected vulnerabilities privately.

## Reporting a vulnerability

Use [GitHub private vulnerability reporting](https://github.com/craigcode/kranz/security/advisories/new).
Do not open a public issue for a vulnerability and do not include live secrets,
customer data, or credentials in a report.

Include the affected version or commit, operating system, configuration,
reproduction steps, observed impact, and any proposed mitigation. You should
receive an acknowledgement within five business days. We will coordinate a
fix, disclosure timing, and credit with you. Please allow a reasonable period
for remediation before public disclosure.

## Supported versions

Security fixes are made on the latest released minor line. The supported public
line begins at v0.2.0. Historical preview artifacts, development snapshots, and
source builds from unmaintained commits are not supported; the private v0.1.0
preview must not be treated as a supported release.

## Operational boundary

Kranz reduces risk; it is not a general-purpose containment boundary. Keep
agent credentials least-privileged, review mission plans and grants, use the
enforced sandbox profiles where supported, and run untrusted repositories in a
separate operating-system or container account.
