# GitHub Actions Maintainer

[![CI](https://github.com/ThreatFlux/github-actions-maintainer/actions/workflows/ci.yml/badge.svg)](https://github.com/ThreatFlux/github-actions-maintainer/actions/workflows/ci.yml)
[![Security](https://github.com/ThreatFlux/github-actions-maintainer/actions/workflows/security.yml/badge.svg)](https://github.com/ThreatFlux/github-actions-maintainer/actions/workflows/security.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.94.0-orange.svg)](https://www.rust-lang.org)

General-purpose GitHub Actions maintenance in Rust, built from the ThreatFlux Rust CI/CD template. The first shipped capabilities are secure pinning, latest-version updates, version-aware status reporting, and authenticated remote pull request creation.

## What It Does

`github-actions-maintainer pin` scans workflow files, finds floating GitHub Action refs such as:

```yaml
- uses: actions/checkout@v4
```

and rewrites them to immutable commit SHAs while keeping the original ref as a comment:

```yaml
- uses: actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd  # v4
```

That preserves operator intent while eliminating runtime drift from moving tags and branches.

## Current Features

- Pins the ref already declared in the workflow instead of upgrading to a newer major automatically
- Supports GitHub-hosted actions with nested paths such as `github/codeql-action/init@v3`
- Skips local actions, Docker actions, and dynamic expressions like `${{ matrix.action }}`
- Offers `--dry-run` for previewing rewrites before touching files
- Can update actions to the latest GitHub release, with tag fallback when releases are absent
- Can report current tracked versions versus latest upstream versions without rewriting files
- Can create a remote branch and pull request with labels instead of rewriting the checked-out repo
- Retries GitHub API calls with exponential backoff and respects `Retry-After` plus rate-limit reset headers
- Validates token scopes before remote PR creation so missing `repo` or `workflow` permissions fail early

## CLI

```bash
cargo run -- pin --dry-run
cargo run -- update --dry-run
cargo run -- update
cargo run -- status
cargo run -- pin --repo /path/to/repo --workflows-path .github/workflows
```

Options:

- `--repo`: repository root to scan, defaults to `.`
- `--workflows-path`: relative workflow directory, defaults to `.github/workflows`
- `--token`: optional GitHub token, also read from `GITHUB_TOKEN`
- `--dry-run`: report rewrites without applying them
- `--create-pr`: create a remote branch and pull request instead of editing files locally
- `--owner` and `--repo-name`: remote repository coordinates for PR creation
- `--labels`, `--title`, `--commit-message`, `--base-branch`, `--branch-name`: control remote PR creation

Command behavior:

- `pin`: pin the ref already declared in the workflow
- `update`: move actions to the latest release or newest tag fallback, then pin them
- `status`: report current tracked versions, latest upstream versions, and whether a change is needed
- `update` without `--create-pr`: apply changes locally in the checked-out repository, which is the equivalent of the original tool's stage mode

Remote update mode:

```bash
cargo run -- update \
  --create-pr \
  --owner ThreatFlux \
  --repo-name githubWorkFlowChecker \
  --token "$GITHUB_TOKEN"
```

Remote update mode will:

- validate the token before mutating repository state
- resolve the default branch when `--base-branch` is not provided
- create a tree/commit/branch through the GitHub API
- open a pull request and attach any requested labels

## Token Permissions

Remote PR mode requires a GitHub token with the equivalent of:

- `repo` or `public_repo`
- `workflow`

`pin`, `update --dry-run`, and `status` can run without a token, but authenticated requests are recommended to raise GitHub API rate limits.

## Rate Limits

The GitHub client retries transient failures and rate-limited responses. It handles:

- `429 Too Many Requests`
- `403 Forbidden` responses that carry rate-limit exhaustion headers
- server-side `5xx` responses
- connection and timeout errors from the HTTP client

When GitHub returns reset metadata, the client sleeps until the reset window instead of blindly retrying.

## GitHub Action Usage

```yaml
name: Pin GitHub Actions

on:
  workflow_dispatch:
  pull_request:
    paths:
      - ".github/workflows/**"

jobs:
  pin-actions:
    runs-on: ubuntu-latest
    permissions:
      contents: read
    steps:
      - uses: actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd # v6.0.2
      - name: Pin workflow actions
        uses: ThreatFlux/github-actions-maintainer@main
        with:
          command: update
          token: ${{ secrets.GITHUB_TOKEN }}
          owner: ${{ github.repository_owner }}
          repo-name: ${{ github.event.repository.name }}
          create-pr: "true"
          dry-run: "false"
```

The repository also ships an [`action.yml`](action.yml) wrapper so the binary can run as a container action.

## Development

```bash
make dev-setup
cargo fmt --all
cargo clippy --all-features --all-targets -- -D warnings
cargo test --all-features
```

## Architecture

The Go-based `githubWorkFlowChecker` concept was narrowed for the initial Rust implementation:

- keep the repo general-purpose for future GitHub Actions maintenance features
- ship secure pinning first, then add version-aware updates and authenticated PR publishing
- separate scanning, GitHub resolution, and rewrite orchestration into small modules

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the current design.
