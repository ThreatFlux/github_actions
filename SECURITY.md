# Security Policy

This repository ships security tooling and workflow hardening logic, so reports should stay private until maintainers can validate and remediate them.

## Supported Versions

| Version | Supported |
| ------- | --------- |
| 0.1.x   | :white_check_mark: |

## Reporting a Vulnerability

**Please do not report security vulnerabilities through public GitHub issues.**

### How to Report

1. **GitHub Security Advisories**: Use the repository Security tab to report privately.
2. **Maintainer contact**: If advisories are not available, coordinate through a private maintainer channel for the repository.

### What to Include

- Type of vulnerability
- Full paths of affected source files
- Location of affected code (tag, branch, commit, or direct URL)
- Step-by-step reproduction instructions
- Proof-of-concept or exploit code if possible
- Impact assessment

### Response Timeline

- **Initial Response**: Within 48 hours
- **Status Update**: Within 5 business days
- **Resolution Target**: Within 90 days

## Operational Safeguards

- GitHub workflow rewrites keep the original ref in comments so version intent remains visible.
- Remote PR mode validates token permissions before attempting branch, commit, or pull request creation.
- GitHub API calls retry on rate limits, transient network failures, and server-side failures with bounded backoff.
- Unauthenticated callers receive a clear error when public API rate limits are exhausted.

### Safe Harbor

We consider security research conducted in good faith to be authorized. We will not pursue legal action against researchers who:

- Make good faith efforts to avoid privacy violations
- Avoid data destruction or service disruption
- Report vulnerabilities promptly
- Allow reasonable time for remediation before disclosure
