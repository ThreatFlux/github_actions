# Releasing

<!--
  RELEASING.md — What makes this document good:

  This is a maintainer runbook for cutting releases. It removes guesswork from
  a high-stakes, infrequent operation and prevents "only Alice knows how to
  release" situations.

  Best practices:
  - Write as a numbered checklist a maintainer can follow step-by-step.
  - Include pre-release validation steps (CI green, changelog updated, etc.).
  - Document both the automated path and the manual fallback.
  - State which secrets / permissions are required and who holds them.
  - Explain what happens after the release (crates.io, Docker, GitHub Release).
  - Keep this under ~100 lines — it should be a runbook, not a tutorial.

  Standard name: RELEASING.md (root or docs/)
  When to include: Any project with a release workflow or published artifacts.
-->

## Automated Release (default)

Releases are driven by [Conventional Commits](https://www.conventionalcommits.org/). When CI and security checks pass on `main`, the `auto-release.yml` workflow runs this repository's own [action](../action.yml) (`uses: ./` with `command: release`), which:

1. Analyzes commits since the last `v*` tag through the GitHub API.
2. Determines the version bump (patch / minor / major) from commit prefixes; chore/docs-only merges produce no release.
3. Rewrites `Cargo.toml` and `Cargo.lock`, commits directly to `main` (fast-forward only), creates the `v*` tag, moves the `v0` major alias tag, and creates the GitHub Release with generated notes.
4. `auto-release.yml` then dispatches `release.yml` (build/package/publish) and `docker.yml` (GHCR images) on the new tag. This dispatch is explicit because tags created with `GITHUB_TOKEN` do not trigger `on: push: tags:` workflows on their own.

**No manual steps are required for routine releases.** A `workflow_dispatch` of `auto-release.yml` with a `version_bump` choice forces a release when no commit qualifies.

## Manual Release

Use this when the automated flow is insufficient (e.g., pre-release versions, hotfixes from a release branch).

### Pre-flight

1. Ensure `main` is green:
   ```bash
   make ci
   ```
2. Update `CHANGELOG.md` — move items from `[Unreleased]` to a new version header.
3. Bump the version in `Cargo.toml`.
4. Commit:
   ```bash
   git add Cargo.toml docs/CHANGELOG.md
   git commit -m "chore: release v1.2.3"
   ```
5. Tag:
   ```bash
   git tag v1.2.3
   git push origin main --tags
   ```

### What Happens Next

The `v*` tag triggers `release.yml`:

| Step | Artifact |
|------|----------|
| Cross-compile | Linux x86_64, Linux aarch64, macOS universal, Windows x86_64 |
| Package | `.tar.gz` (Unix) and `.zip` (Windows) with binary + LICENSE + README |
| Publish | crates.io (if `CRATES_IO_TOKEN` secret is set) |
| GitHub Release | Checksums + packaged assets attached |

The `docker.yml` workflow also triggers on the tag, producing:

| Step | Artifact |
|------|----------|
| Build | Multi-arch Docker image |
| Scan | Trivy vulnerability scan |
| Sign | Cosign image signature |
| SBOM | CycloneDX image SBOM |
| Push | `ghcr.io/threatflux/<image>:<tag>` |

### Required Permissions

| Secret | Holder | Purpose |
|--------|--------|---------|
| `GITHUB_TOKEN` | Automatic | Release assets, GHCR push |
| `CRATES_IO_TOKEN` | Repo admin | crates.io publish |

### Rollback

If a release is defective:

1. Delete the GitHub Release (draft state or full delete).
2. Delete the Git tag: `git push --delete origin v1.2.3`
3. Yank from crates.io if published: `cargo yank --version 1.2.3`
4. Fix, then re-release with the next patch version.
