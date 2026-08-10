# Architecture

`github-actions-maintainer` is structured as a small Rust library plus a thin CLI wrapper.

## Current Feature Set

- `pin`: scans workflow YAML files for `uses:` references that point at GitHub-hosted actions
- resolves each floating ref such as `actions/checkout@v4` to the commit SHA GitHub currently serves for that ref
- rewrites the workflow to `owner/repo[/path]@<sha>  # <original-ref>`
- `update`: resolves the latest release tag, or newest tag as a fallback, then rewrites workflows to that version and SHA
- `status`: reports current tracked version versus latest upstream version without modifying files
- `policy`: reports explicit Bash/Python script usage and baseline workflow policy findings without modifying files
- `release`: computes the next version from conventional commits, then publishes the release commit, tag, and GitHub Release, or stages them on a release pull request
- one container action (root `action.yml`) exposes all five commands through its `command` input
- remote PR mode: stages updated workflow content into GitHub blobs, trees, commits, branch refs, and a pull request without shelling out to `git`

That keeps the original intent visible while making the executed dependency immutable.

## Module Layout

- `src/github.rs`: blocking GitHub API client used to resolve refs, discover latest releases, retry through rate limits, and perform remote repository mutations (branches, tags, commits, releases)
- `src/workflow.rs`: workflow discovery, `uses:` scanning, and line-oriented rewrites
- `src/model.rs`: shared domain types for scanned actions, script findings, policy findings, and rewrite reports
- `src/pinning.rs`: orchestration layer for conservative pinning of the existing ref
- `src/policy.rs`: read-only workflow policy and script-usage scanning
- `src/update.rs`: version-aware update and status orchestration
- `src/cargo.rs`: Cargo manifest scanning and registry-backed dependency updates
- `src/crates_io.rs`: blocking crates.io API client with retry handling
- `src/conventional.rs`: conventional-commit classification and semver bump computation
- `src/versioning.rs`: release version rewriting across workspace manifests and `Cargo.lock`
- `src/release.rs`: release orchestration — commit range analysis, race-guarded commit/tag/release publishing, and notes generation
- `src/remote.rs`: remote branch, commit, and pull request publishing for update mode
- `src/input_env.rs`: normalizes the container action's `INPUT_<NAME>` variables and empty flag values before clap parses the command line
- `src/main.rs`: Clap-based CLI entrypoint

## Intentional Boundaries

- `pin` pins the ref already present in the workflow.
- `update` intentionally upgrades to the latest release or tag instead of preserving the existing major.
- `update --create-pr` computes changes locally first, then publishes them through the GitHub API.
- Local actions, Docker actions, and dynamic matrix expressions are skipped by the pinning and update commands.
- `policy` is read-only and intentionally line-oriented, so it can run without mutating repository workflows.
- Rewrites are line-oriented to preserve the rest of the workflow file exactly.
