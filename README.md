# GitHub Actions Maintainer

[![CI](https://github.com/ThreatFlux/github-actions-maintainer/actions/workflows/ci.yml/badge.svg)](https://github.com/ThreatFlux/github_actions/actions/workflows/ci.yml)
[![Security](https://github.com/ThreatFlux/github-actions-maintainer/actions/workflows/security.yml/badge.svg)](https://github.com/ThreatFlux/github_actions/actions/workflows/security.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.97.1-orange.svg)](https://www.rust-lang.org)

General-purpose dependency maintenance in Rust, built from the ThreatFlux Rust CI/CD template. The shipped capabilities cover secure GitHub Action pinning plus latest-version reporting and updates for both GitHub Actions and cargo packages.

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
- Can scan `Cargo.toml` manifests, find the latest stable crates.io versions, and update supported cargo dependency requirements
- Reports unmanaged cargo dependency shapes such as `path`, `git`, and `workspace = true` entries instead of rewriting them
- Can create a remote branch and pull request with labels instead of rewriting the checked-out repo
- Can scan workflows for explicit Bash/sh and Python usage in `run:` blocks and `shell:` declarations
- Can report baseline workflow policy findings for unpinned actions, missing explicit permissions, write-level permissions, and missing job timeouts
- Retries GitHub API calls with exponential backoff and respects `Retry-After` plus rate-limit reset headers
- Validates token scopes before remote PR creation so missing `repo` or `workflow` permissions fail early

## CLI

```bash
cargo run -- pin --dry-run
cargo run -- update --dry-run
cargo run -- update --cargo --dry-run
cargo run -- update --all
cargo run -- update
cargo run -- status
cargo run -- status --cargo
cargo run -- policy
cargo run -- policy --fail-on-findings
cargo run -- pin --repo /path/to/repo --workflows-path .github/workflows
```

Options:

- `--repo`: repository root to scan, defaults to `.`
- `--workflows-path`: relative workflow directory, defaults to `.github/workflows`
- `--token`: optional GitHub token, also read from `GITHUB_TOKEN`
- `--dry-run`: report rewrites without applying them
- `--cargo`: target cargo package dependencies
- `--github-actions`: target GitHub Actions updates explicitly
- `--all`: target both GitHub Actions and cargo package dependencies
- `--create-pr`: create a remote branch and pull request instead of editing files locally
- `--owner` and `--repo-name`: remote repository coordinates for PR creation
- `--labels`, `--title`, `--commit-message`, `--base-branch`, `--branch-name`: control remote PR creation
- `--check-scripts` / `--check-policies`: enable or disable the `policy` script and policy scans, both enabled by default
- `--fail-on-findings`: make `policy` exit non-zero when it finds script usage or policy violations
- `--extra-files`: comma-separated files to stage into the release commit alongside the version rewrites

Command behavior:

- `pin`: pin the ref already declared in the workflow
- `update`: move selected dependencies to the latest upstream version. By default it targets GitHub Actions; add `--cargo` or `--all` for cargo support
- `status`: report current tracked versions, latest upstream versions, and whether a change is needed for the selected target set
- `policy`: scan workflow files for explicit Bash/Python script usage and baseline workflow policy findings without modifying files
- `update` without `--create-pr`: apply changes locally in the checked-out repository, which is the equivalent of the original tool's stage mode
- `release`: bump the Cargo version from conventional commits, then create the release commit, tag, and GitHub Release through the API (see below)

Cargo update support currently manages registry-backed dependencies that declare a direct version requirement such as:

- `reqwest = "0.12.13"`
- `serde = { version = "^1.0.200", features = ["derive"] }`
- `regex = { version = "~1.10.0" }`

The updater preserves the existing requirement operator where possible and skips unsupported forms such as multi-range requirements, `path` dependencies, `git` dependencies, and `workspace = true` references.

Policy scanning reports:

- unpinned action references that do not use a full 40-character commit SHA (high)
- `permissions: write-all` (high) and other write-carrying shorthand values (medium)
- individual `<scope>: write` permission entries, with `id-token: write` treated as low
- workflows with no explicit top-level `permissions` block (medium)
- jobs with no `timeout-minutes` (medium)

Script scanning reports explicit Bash/sh and Python usage, both from `shell:` declarations and from interpreter invocations inside `run:` blocks. Both scans are read-only; pair them with `--fail-on-findings` to gate a pull request.

Remote update mode:

```bash
cargo run -- update \
  --cargo \
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

Release mode:

```bash
# Outside GitHub Actions, pass --owner/--repo-name (or set GITHUB_REPOSITORY).
cargo run -- release --dry-run \
  --owner ThreatFlux \
  --repo-name github_actions \
  --token "$GITHUB_TOKEN"
cargo run -- release \
  --owner ThreatFlux \
  --repo-name github_actions \
  --update-major-alias \
  --token "$GITHUB_TOKEN"
```

`release` runs entirely through the GitHub REST API — no `git`, `gh`, or `cargo` binaries are needed at runtime — so it works inside minimal containers:

1. Reads the current version from `Cargo.toml` (`[package].version`, falling back to `[workspace.package].version`).
2. Finds the latest `--tag-prefix` semver tag and classifies the conventional commits since it (`feat:` → minor, `fix:` → patch, `!`/`BREAKING CHANGE` → major). Merge commits are skipped. When no commit warrants a release it exits successfully with `released=false`; `--bump major|minor|patch` forces a release.
3. Rewrites the version across workspace member manifests, internal dependency pins, and `Cargo.lock`.
4. Creates the release commit directly on the base branch (fast-forward only — if the branch advanced past the analyzed head, the run skips cleanly), the `vX.Y.Z` tag (annotated by default so provenance checks like `git cat-file -t` pass; `--tag-style lightweight` opts out), an optional moving major alias tag (`--update-major-alias`), and the GitHub Release with grouped release notes.
5. Writes release notes to `--notes-file` and `released`/`version`/`tag`/`release-url`/`notes-file` outputs to `$GITHUB_OUTPUT` when set.

Release mode requires a token with `contents: write` on the target repository. The `workflow` scope is not required because release commits only touch Cargo manifests.

## Token Permissions

Remote PR mode requires a GitHub token with the equivalent of:

- `repo` or `public_repo`
- `workflow`

`pin`, `update --dry-run`, and `status` can run without a token. Authenticated requests are still recommended for GitHub-backed operations to raise API rate limits.

## Rate Limits

The GitHub client retries transient failures and rate-limited responses. The crates.io client also retries `429` and `5xx` responses with `Retry-After` handling.

GitHub handling includes:

- `429 Too Many Requests`
- `403 Forbidden` responses that carry rate-limit exhaustion headers
- server-side `5xx` responses
- connection and timeout errors from the HTTP client

When GitHub returns reset metadata, the client sleeps until the reset window instead of blindly retrying.

## GitHub Action Usage

One action ships every command. Reference it as `ThreatFlux/github_actions@<ref>`
(the root [`action.yml`](action.yml)) and select the behavior with `command`:

| `command` | What it does |
|---|---|
| `pin` | Rewrite floating action refs in workflow files to the commit SHA they resolve to today. |
| `update` | Move GitHub Actions and/or cargo dependencies to their latest upstream version, locally or on a pull request. |
| `status` | Report current versus latest versions without writing anything. |
| `policy` | Report script usage and baseline workflow policy findings without writing anything. |
| `release` | Bump the Cargo version from conventional commits and publish the release commit, tag, and GitHub Release — or stage them on a release pull request. |

The action is a Docker container action built from
[`runtime/Dockerfile`](runtime/Dockerfile), which pulls a digest-pinned
prebuilt image, so it starts in seconds instead of compiling from source.

### Inputs

| Input | Commands | Default | Description |
|---|---|---|---|
| `command` | all | `pin` | Command to run: `pin`, `update`, `status`, `policy`, or `release`. |
| `token` | all | `${{ github.token }}` | GitHub token. Required for remote pull request creation and for `release`; recommended everywhere to raise API rate limits. |
| `owner` / `repo-name` | all | from `GITHUB_REPOSITORY` | Target repository coordinates. |
| `repo` | all | `.` | Path to the checked-out repository. |
| `base-branch` | `update`, `release` | repository default branch | Base of the dependency pull request, or the branch to release from. |
| `dry-run` | all | `false` | Analyze and report without writing files, commits, tags, releases, or pull requests. |
| `create-pr` | `update`, `release` | `false` | Open a dependency-update pull request, or stage the release on a release pull request instead of publishing directly. |
| `commit-message` | `update`, `release` | per command | `Update dependencies` for `update`; `chore: release v{version}` for `release`. |
| `workflows-path` | `pin`, `update`, `status`, `policy` | `.github/workflows` | Workflow directory relative to the repository root. |
| `github-actions` | `update`, `status` | `false` | Include GitHub Actions workflow updates. |
| `cargo` | `update`, `status` | `false` | Include cargo package dependency updates. |
| `all` | `update`, `status` | `false` | Include both GitHub Actions and cargo updates. |
| `branch-name` | `update` | generated | Branch name for the dependency-update pull request. |
| `labels` | `update` | `dependencies` | Comma-separated labels for the dependency-update pull request. |
| `title` | `update` | `Update dependencies` | Title for the dependency-update pull request. |
| `check-scripts` | `policy` | `true` | Report explicit Bash/sh and Python usage in `run:` and `shell:` blocks. |
| `check-policies` | `policy` | `true` | Report unpinned actions, permission, and job timeout findings. |
| `fail-on-findings` | `policy` | `false` | Fail the action when the scan reports any finding. |
| `bump` | `release` | `auto` | `auto`, `major`, `minor`, or `patch`. |
| `tag-prefix` | `release` | `v` | Prefix for release tags. |
| `tag-style` | `release` | `annotated` | `annotated` or `lightweight`. |
| `update-major-alias` | `release` | `false` | Also move the moving major alias tag (for example `v0`). |
| `notes-file` | `release` | `release_notes.md` | Where generated release notes are written, including on dry runs. |
| `release-branch` | `release` | `automation/release` | Automation-owned branch used with `create-pr`; must use the `automation/release` prefix. |
| `extra-files` | `release` | none | Comma-separated repository-relative files to stage into the release commit, for values that can only be resolved at release time. |

### Outputs

Every output is set by `release` and is empty for the other commands.

| Output | Description |
|---|---|
| `released` | `true` when a release was created, otherwise `false`. |
| `version` | Released version without the tag prefix (also set on dry runs and tag-exists skips). |
| `tag` | Created release tag. |
| `release-url` | URL of the created GitHub Release. |
| `notes-file` | Path to the generated notes file, empty when no notes were generated. |
| `release-pr-number` | Number of the created or updated release pull request. |
| `release-pr-url` | URL of the created or updated release pull request. |
| `release-branch` | Branch used for the release pull request. |

### `command: pin`

```yaml
jobs:
  pin:
    runs-on: ubuntu-latest
    permissions:
      contents: read
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - name: Pin workflow action refs
        uses: ThreatFlux/github_actions@v0 # pin to a SHA in production
        with:
          command: pin
          token: ${{ secrets.GITHUB_TOKEN }}
```

`pin` rewrites the checked-out files in place; commit them yourself, or add
`dry-run: "true"` to report the rewrites without touching the working tree.

### `command: status`

```yaml
      - name: Report dependency drift
        uses: ThreatFlux/github_actions@v0 # pin to a SHA in production
        with:
          command: status
          all: "true"
          token: ${{ secrets.GITHUB_TOKEN }}
```

`status` never writes; `contents: read` is enough.

### `command: policy`

```yaml
      - name: Scan workflow policy
        uses: ThreatFlux/github_actions@v0 # pin to a SHA in production
        with:
          command: policy
          fail-on-findings: "true"
```

`policy` reads only the checked-out workflow files, so it needs no token and
`contents: read` is enough. Narrow the scan with `check-scripts: "false"` or
`check-policies: "false"`; drop `fail-on-findings` to report without failing the
job.

### `command: update`

```yaml
jobs:
  update:
    runs-on: ubuntu-latest
    permissions:
      contents: read
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - name: Update workflow and cargo dependencies
        uses: ThreatFlux/github_actions@v0 # pin to a SHA in production
        with:
          command: update
          all: "true"
          create-pr: "true"
          token: ${{ secrets.DEPENDENCY_UPDATE_TOKEN }}
          owner: ${{ github.repository_owner }}
          repo-name: ${{ github.event.repository.name }}
```

`create-pr: "true"` publishes through the GitHub API rather than editing the
checkout, so the job itself needs no write permission — but the *token* does.
Updating files under `.github/workflows/` requires the `workflow` scope, which
the default `GITHUB_TOKEN` does not have; use a PAT or GitHub App token there.
Without `create-pr`, `update` edits the checked-out files and you commit them.
`create-pr` is one of the inputs affected by the
[version skew note](#version-skew-during-upgrades) below.

### `command: release`

Direct mode publishes the release commit, tag, and GitHub Release:

```yaml
jobs:
  release:
    runs-on: ubuntu-latest
    permissions:
      contents: write
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - id: release
        uses: ThreatFlux/github_actions@v0 # pin to a SHA in production
        with:
          command: release
          token: ${{ secrets.GITHUB_TOKEN }}
          update-major-alias: "true"
      - if: steps.release.outputs.released == 'true'
        run: echo "Released ${{ steps.release.outputs.tag }} -> ${{ steps.release.outputs.release-url }}"
```

Release-pull-request mode stages the version bump on the automation-owned
branch and opens or refreshes one pull request instead of publishing:

```yaml
jobs:
  release-pr:
    runs-on: ubuntu-latest
    permissions:
      contents: write
      pull-requests: write
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - id: release
        uses: ThreatFlux/github_actions@v0 # pin to a SHA in production
        with:
          command: release
          token: ${{ secrets.GITHUB_TOKEN }}
          create-pr: "true"
          release-branch: automation/release
      - name: Report the release pull request
        run: |
          echo "PR #${{ steps.release.outputs.release-pr-number }} on ${{ steps.release.outputs.release-branch }}"
          echo "${{ steps.release.outputs.release-pr-url }}"
```

In pull-request mode no tag moves and no GitHub Release is published, so
`released` stays `false` and `release-pr-number`/`release-pr-url`/`release-branch`
carry the result; the tag and Release are cut by the follow-on release run
after the pull request merges. Merges whose commits are only `chore:`/`docs:`
produce no release at all — the action exits successfully with
`released=false`, which is also what keeps release commits from looping.

### Tokens and downstream pipelines

GitHub suppresses workflow triggers for events created with the default
`GITHUB_TOKEN`: the tag `release` creates will **not** start your tag-triggered
(`on: push: tags:`) workflows. Pick one of:

1. **Same-workflow chaining (no extra secrets):** gate follow-on jobs on
   `steps.release.outputs.released == 'true'`, or dispatch the tag pipelines
   explicitly — `workflow_dispatch` is exempt from the suppression rule. The
   dispatching job needs `permissions: actions: write`.
2. **GitHub App or PAT token:** pass it as `token` and the created tag triggers
   `on: push: tags:` workflows natively.
3. **No downstream pipelines:** the default `GITHUB_TOKEN` is all you need.

`release` needs a token with `contents: write` (plus `pull-requests: write` for
`create-pr`). The `workflow` scope is not required, because release commits
only touch Cargo manifests.

### Branch protection

Direct-mode `release` pushes the release commit to the base branch
(fast-forward only). On a protected branch, add a bypass for the identity the
token represents — for rulesets, add the GitHub Actions app or your own GitHub
App to the bypass list. `create-pr: "true"` is the alternative that respects
branch protection: it never writes to the protected branch directly.

The action pins its analysis to the branch head it first observes and publishes
with a fast-forward-only ref update, so a branch that advances mid-run makes the
run skip cleanly with `released=false` instead of releasing a stale commit. If
the computed tag already exists, the run skips as well. Run release jobs under a
`concurrency` group regardless.

### GitHub App authentication

To attribute release commits and pull requests to an App instead of
`github-actions[bot]`:

1. Create a GitHub App under your organization and generate a private key.
2. Grant the installation **Contents: Read and write**, **Pull requests: Read
   and write**, and **Actions: Read and write**, then install it on every
   repository that releases.
3. Add the App's numeric ID as the `RELEASE_APP_ID` repository or organization
   variable.
4. Add the private-key PEM as the `RELEASE_APP_PRIVATE_KEY` secret. Never commit
   the PEM or store it in a plain-text variable.
5. Pass `github-app-id` and the `github-app-private-key` secret to the reusable
   workflow below, which mints the installation token with
   `actions/create-github-app-token` and hands it to the action.

App authentication attributes API commits and pull requests to the App;
cryptographic commit signing still requires a separate signing-key policy.
Configure both values together — a half-configured App fails the workflow
instead of silently falling back.

## Reusable Auto Release Workflow

For Cargo repositories that want the whole release gate — required-workflow
checks, optional GitHub App authentication, and downstream workflow dispatches
— call the reusable workflow instead of wiring the action yourself:

```yaml
name: Auto Release
on:
  push:
    branches: [main]
concurrency:
  group: auto-release-${{ github.ref }}
permissions:
  contents: read
  actions: read
  pull-requests: read
jobs:
  release:
    permissions:
      contents: write
      actions: write
      pull-requests: write
    uses: ThreatFlux/github_actions/.github/workflows/reusable-auto-release.yml@v0 # pin to a SHA in production
    with:
      bump: auto
```

The reusable workflow needs no separate action pin: it checks out and runs the
action at its own commit (`job.workflow_sha`), so the action version always
matches whatever workflow ref you pinned. Its inputs, outputs, and the
`required-workflows` / `dispatch-workflows` gate are documented in
[release/README.md](release/README.md#reusable-workflow-inputs).

## Migrating to the Unified Action

This repository used to ship two actions. It now ships one; `release/` is
deprecated.

| Before | After |
|---|---|
| `uses: ThreatFlux/github_actions/release@v0` | `uses: ThreatFlux/github_actions@v0` plus `command: release` |
| `uses: ThreatFlux/github_actions@v0` (maintainer) | unchanged, but state `command:` explicitly — it defaults to `pin` |

Every `release/` input keeps its name and default on the unified action, and
all eight release outputs are unchanged, so migrating is the two-line edit
above. Root-action users who were already passing inputs such as
`workflows-path`, `all`, `labels`, or `title` should read the version-skew note
below: those inputs now travel to the binary as environment variables and need
runtime image 0.6.1 or newer to take effect.

### Version skew during upgrades

Only the flags every published binary accepts are passed as container
arguments (`command`, `repo`, `token`, `owner`, `repo-name`, `base-branch`,
`dry-run`). Every other input reaches the binary through the `INPUT_<NAME>`
environment variables GitHub sets for container actions. That keeps the action
working while [`runtime/Dockerfile`](runtime/Dockerfile) still pins a pre-0.6.0
image — but that older binary ignores `INPUT_*` variables entirely.

Until the first post-merge release (0.6.0) publishes and Dependabot bumps the
`/runtime` pin (yielding 0.6.1), these inputs silently fall back to their
built-in defaults:

- `pin`, `update`, `status`: `workflows-path`, `github-actions`, `cargo`,
  `all`, `branch-name`, `labels`, `title`, `commit-message`, `create-pr`
- `release`: `bump`, `tag-prefix`, `tag-style`, `update-major-alias`,
  `notes-file`, `release-branch`, `commit-message`, `create-pr`

Most notably, **`create-pr: "true"` on `release` performs a direct release
instead of opening a release pull request** during that window, and
`create-pr: "true"` on `update` rewrites the checkout instead of opening a
dependency pull request. Pin the action
to a ref whose `runtime/Dockerfile` holds 0.6.1 or newer before relying on any
of these inputs. The skew self-heals once that pin lands.

### Deprecation timeline

`ThreatFlux/github_actions/release@<ref>` still works and still takes the same
inputs, but it is deprecated as of the unified action and will be **removed in
the next major version**. New workflows should use `command: release` on the
root action; existing ones have the whole `v0` line to migrate.

Roadmap: the release engine is manifest-driven (`src/versioning.rs`), with Cargo supported today; npm (`package.json`) and Python (`pyproject.toml`) manifest adapters are planned next so the same action covers the whole ThreatFlux org.

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
