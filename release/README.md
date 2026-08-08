# ThreatFlux Auto Release Action

Automatic Cargo releases on merge to main: bump the version from
[Conventional Commits](https://www.conventionalcommits.org/), rewrite
`Cargo.toml`/`Cargo.lock`, and create the release commit, tag, and GitHub
Release with generated notes — entirely through the GitHub REST API. No `git`,
`gh`, or `cargo` binaries run at release time, and the action starts in
seconds because it pulls a prebuilt image instead of compiling from source.

## Quick Start

Reusable workflow (recommended):

```yaml
name: Auto Release
on:
  workflow_run:
    workflows: [CI, Security]
    types: [completed]
    branches: [main]
  workflow_dispatch:
concurrency:
  group: auto-release-${{ github.ref }}
  cancel-in-progress: false

permissions:
  contents: read
  actions: read
  pull-requests: read
jobs:
  release:
    permissions:
      contents: write
      actions: write # required when dispatch-workflows is used
      pull-requests: write # required when create-pr is true
    uses: ThreatFlux/github_actions/.github/workflows/reusable-auto-release.yml@v0 # pin to a SHA in production
    with:
      bump: auto
      required-workflows: CI,Security
      dispatch-workflows: release.yml,docker.yml
      dispatch-version-workflow: release.yml
      # Optional: authenticate release commits/PRs as an installed GitHub App.
      github-app-id: ${{ vars.RELEASE_APP_ID }}
    secrets:
      github-app-private-key: ${{ secrets.RELEASE_APP_PRIVATE_KEY }}
      # release-token: ${{ secrets.RELEASE_TOKEN }}   # optional PAT/App token fallback
```

Raw action:

```yaml
name: Auto Release
on:
  push:
    branches: [main]
concurrency:
  group: auto-release-${{ github.ref }}
  cancel-in-progress: false
permissions:
  contents: write
  pull-requests: write # required when create-pr is true
jobs:
  release:
    runs-on: ubuntu-latest
    if: "!startsWith(github.event.head_commit.message, 'chore: release')"
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - id: release
        uses: ThreatFlux/github_actions/release@v0 # pin to a SHA in production
        with:
          token: ${{ secrets.GITHUB_TOKEN }}
      - if: steps.release.outputs.released == 'true'
        run: echo "Released ${{ steps.release.outputs.tag }} -> ${{ steps.release.outputs.release-url }}"
```

Merges whose commits are only `chore:`/`docs:`/etc. produce no release — the
action exits successfully with `released=false`. That no-op is also what stops
release commits from triggering an endless release loop.

## Automated release pull requests

Set `create-pr: true` to stage the version update on the reserved
`automation/release` branch and create or refresh one pull request against the
base branch:

```yaml
- uses: ThreatFlux/github_actions/release@<pinned-sha>
  with:
    token: ${{ secrets.GITHUB_TOKEN }}
    create-pr: true
    release-branch: automation/release
```

PR mode requires `contents: write` and `pull-requests: write`, and force-refreshes that automation-owned branch from the analyzed base,
then opens or updates the matching open PR. It does not move tags or publish a
GitHub Release; those happen in the follow-on release workflow after merge.
The action exposes `release-pr-number`, `release-pr-url`, and `release-branch`
outputs, while `released` remains `false`.

## Reusable workflow inputs

The reusable workflow keeps consumer files small while retaining the common
release safeguards. `required-workflows` is a comma-separated list of workflow
names that must have a successful run for the target commit, regardless of the
event that triggered that run. `dispatch-workflows` is a
comma-separated list of workflow files to run after a release; set
`dispatch-version-workflow` when one of them accepts a `version` input. These
features replace the duplicated `gh run list` and `gh workflow run` shell
scripts that would otherwise live in every repository.

### GitHub App authentication

To configure App authentication for a repository:

1. Create a GitHub App under your organization (or use an existing App) and
   generate a private key.
2. Grant the App installation repository permissions: **Contents: Read and
   write**, **Pull requests: Read and write**, and **Actions: Read and write**.
   Install the App on every repository that will call this workflow.
3. Add the App's numeric ID as the repository or organization variable
   `RELEASE_APP_ID`.
4. Add the downloaded private-key PEM as the repository or organization secret
   `RELEASE_APP_PRIVATE_KEY`. Never commit the PEM or put it in a plain-text
   variable.
5. Pass both values to the reusable workflow as shown in Quick Start. Keep the
   caller's job permissions sufficient for the called workflow.

The reusable workflow mints an installation token with
`actions/create-github-app-token`.
The release action and downstream workflow dispatches then authenticate as the
App. The App installation must have repository `contents: write`,
`pull_requests: write`, and `actions: write` permissions. If these values are
not configured, the workflow falls back to `release-token` and finally the
default `GITHUB_TOKEN`. This authenticates and attributes API commits and PRs
to the App; cryptographic commit signing still requires a separate signing-key
policy on the repository.

The App token is scoped to the current repository. Rotate the private key by
replacing the secret and revoke old keys in the App settings. If App
configuration is absent or token creation fails, the workflow does not silently
fall back from a partially configured App; validate the App ID, installation,
and secret before enabling it in a protected release workflow.
## How Versions Are Computed

Commits since the latest `<tag-prefix>X.Y.Z` tag are classified by their
subject line:

| Commit | Bump |
|---|---|
| `feat!: ...`, `refactor(core)!: ...`, or a `BREAKING CHANGE:` footer | major |
| `feat: ...` | minor |
| `fix: ...` | patch |
| anything else | no release |

The strongest match wins. The bump applies to the version in `Cargo.toml`
(`[package].version`, falling back to `[workspace.package].version`), and the
rewrite covers workspace member manifests, internal dependency version pins,
and `Cargo.lock` entries for workspace packages. Merge commits are ignored.
`bump: major|minor|patch` forces a release when no commit qualifies.

## Inputs

| Input | Default | Description |
|---|---|---|
| `token` | `${{ github.token }}` | Token used for analysis and release creation. Needs `contents: write`. |
| `owner` / `repo-name` | from `GITHUB_REPOSITORY` | Target repository coordinates. |
| `repo` | `.` | Path to the checked-out repository (source of `Cargo.toml`). |
| `base-branch` | repository default branch | Branch to release from. |
| `bump` | `auto` | `auto`, `major`, `minor`, or `patch`. |
| `tag-prefix` | `v` | Prefix for release tags. |
| `update-major-alias` | `false` | Also move the moving major alias tag (for example `v1`) to the release commit. |
| `notes-file` | `release_notes.md` | Where generated release notes are written (also on dry runs). |
| `commit-message` | `chore: release v{version}` | Release commit message template. |
| `create-pr` | `false` | Create or update an automated release pull request instead of publishing directly. |
| `release-branch` | `automation/release` | Automation-owned branch; must use the `automation/release` prefix. |
| `github-app-id` | empty | Optional App ID used with the `github-app-private-key` secret. |
| `dry-run` | `false` | Analyze and report without creating anything. |

## Outputs

| Output | Description |
|---|---|
| `released` | `true` when a release was created, otherwise `false`. |
| `version` | Version without the tag prefix (also set on dry runs and tag-exists skips). |
| `tag` | Release tag name. |
| `release-url` | URL of the created GitHub Release. |
| `release-pr-number` | Number of the created or updated automated release pull request. |
| `release-pr-url` | URL of the created or updated automated release pull request. |
| `release-branch` | Branch used for the automated release pull request. |
| `notes-file` | Path to the generated notes file, empty when no notes were generated. |

## Tokens and Downstream Pipelines

GitHub suppresses workflow triggers for events created with the default
`GITHUB_TOKEN`: the tag this action creates will **not** start your
tag-triggered (`on: push: tags:`) workflows. Pick one of:

1. **Same-workflow chaining (no extra secrets):** run follow-on jobs in the
   same workflow gated on `needs.release.outputs.released == 'true'`, or
   dispatch tag pipelines explicitly — `workflow_dispatch` is exempt from the
   suppression rule:

   ```yaml
   - if: steps.release.outputs.released == 'true'
     env:
       GH_TOKEN: ${{ github.token }}
     run: gh workflow run release.yml --ref "${{ steps.release.outputs.tag }}"
   ```

   The dispatching job needs `permissions: actions: write`.

2. **GitHub App or PAT token:** pass it as `token` and the created tag
   triggers `on: push: tags:` workflows natively.

3. **No downstream pipelines:** the default `GITHUB_TOKEN` is all you need.

## Branch Protection

The action pushes the release commit directly to the base branch
(fast-forward only). On a protected branch, add a bypass for the identity the
token represents — for rulesets, add the GitHub Actions app (or your GitHub
App) to the bypass list. A PR-based fallback mode is on the roadmap.

## Concurrency and Races

Run the job under a `concurrency` group (see Quick Start). Independently of
that, the action pins its analysis to the branch head it first observes and
publishes with a fast-forward-only ref update: if the branch advances
mid-run, the run skips cleanly with `released=false` instead of releasing a
stale commit. If the computed tag already exists, the run also skips (exit 0)
rather than editing the existing release.

## Pinning This Action

Pin by commit SHA with a version comment, consistent with how this repository
pins its own actions:

```yaml
uses: ThreatFlux/github_actions/release@<40-char-sha> # vX.Y.Z
```

The moving `v0` (later `v1`) alias tag is maintained by this repository's own
release automation and is the convenience alternative.

The reusable workflow needs no separate action pin: it checks out and runs the
release action at its own commit (`job.workflow_sha`), so the action version
always matches whatever workflow ref you pinned.
