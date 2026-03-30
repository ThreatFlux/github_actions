# Architecture

`github-actions-maintainer` is structured as a small Rust library plus a thin CLI wrapper.

## Current Feature Set

- `pin`: scans workflow YAML files for `uses:` references that point at GitHub-hosted actions
- resolves each floating ref such as `actions/checkout@v4` to the commit SHA GitHub currently serves for that ref
- rewrites the workflow to `owner/repo[/path]@<sha>  # <original-ref>`
- `update`: resolves the latest release tag, or newest tag as a fallback, then rewrites workflows to that version and SHA
- `status`: reports current tracked version versus latest upstream version without modifying files
- remote PR mode: stages updated workflow content into GitHub blobs, trees, commits, branch refs, and a pull request without shelling out to `git`

That keeps the original intent visible while making the executed dependency immutable.

## Module Layout

- `src/github.rs`: blocking GitHub API client used to resolve refs, discover latest releases, retry through rate limits, and perform remote repository mutations
- `src/workflow.rs`: workflow discovery, `uses:` scanning, and line-oriented rewrites
- `src/model.rs`: shared domain types for scanned actions and rewrite reports
- `src/pinning.rs`: orchestration layer for conservative pinning of the existing ref
- `src/update.rs`: version-aware update and status orchestration
- `src/remote.rs`: remote branch, commit, and pull request publishing for update mode
- `src/main.rs`: Clap-based CLI entrypoint

## Intentional Boundaries

- `pin` pins the ref already present in the workflow.
- `update` intentionally upgrades to the latest release or tag instead of preserving the existing major.
- `update --create-pr` computes changes locally first, then publishes them through the GitHub API.
- Local actions, Docker actions, and dynamic matrix expressions are skipped.
- Rewrites are line-oriented to preserve the rest of the workflow file exactly.
