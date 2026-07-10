# Security Policy

## Supported versions

Before the first stable release, security fixes are applied to the latest code on `main` and the most recent published `0.x` release when practical. Older pre-release versions are not supported.

## Reporting a vulnerability

Do not open a public issue for a suspected vulnerability.

Use GitHub's [private vulnerability reporting form](https://github.com/benkleyner/ctcx/security/advisories/new) to send the report privately. Include:

- The affected command or configuration behavior.
- Reproduction steps or a minimal repository.
- The expected security impact.
- Any suggested mitigation, if known.

You should receive an acknowledgement within seven days. The maintainer will coordinate validation, remediation, and disclosure through the private advisory.

Particularly relevant reports include project-root escapes, unsafe generated-file replacement, malicious import handling, and unexpected execution of instruction content.
