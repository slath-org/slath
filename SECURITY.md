<!-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de> -->
<!-- SPDX-License-Identifier: CC-BY-SA-4.0 -->
# Security policy

Please don't report security vulnerabilities in public issues, pull requests,
or discussions.

## Reporting a vulnerability

Report privately by one of these routes:

- **GitHub:** [open a private security advisory](https://github.com/slath-org/slath/security/advisories/new).
  This is the preferred route.
- **Email:** <dev@erichseifert.de>, with "Slath security" in the subject.

Please include:

- the affected version or commit,
- what an attacker can do, and what access they need first (anonymous,
  signed-in user, repository member, administrator, CI job),
- steps to reproduce, or a proof of concept,
- any configuration that matters (reverse proxy, SSH setup, runner setup).

If you aren't sure whether something is a vulnerability, report it privately
anyway.

## What happens next

- We acknowledge your report within **3 business days**.
- We confirm or reject the issue and tell you what we assess its severity to be.
- For high and critical issues, we aim to release a fix within **30 days**.
  Lower-severity issues are fixed as part of normal development.
- We agree a disclosure date with you. By default, we publish an advisory
  when the fix is released, and no later than 90 days after your report.
- We credit you in the advisory unless you prefer not to be named.

Please give us a reasonable chance to fix the issue before you disclose it.

## Supported versions

| Version | Supported |
| --- | --- |
| 0.2.x | Yes |
| 0.1.x | No |
| `main` | Yes |

Until 1.0, security fixes go into the latest minor release only. From 1.0
on, they go into the latest two minor releases.

## Scope

In scope is everything in this repository: the `slath` server and its helper
subcommands, the web UI, the API, Git access over HTTP and SSH, the shipped
`sshd` and deployment configuration, and the documented defaults.

Out of scope:

- Vulnerabilities in third-party software Slath uses, such as Git, OpenSSH,
  PostgreSQL, or the CI runner. Please report those upstream. Tell us too if
  Slath's use of it makes the problem worse.
- Untrusted code running in a CI job on a container runner the operator has
  configured to accept such jobs. Slath documents that containers aren't an
  isolation boundary for untrusted code. Escapes from Slath's own controls
  around CI (approvals, secrets, tokens) are in scope.
- Problems that need an attacker who already has administrator access to the
  Slath instance or shell access to its host.
- Missing hardening without a concrete attack, and reports produced only by
  automated scanners.
