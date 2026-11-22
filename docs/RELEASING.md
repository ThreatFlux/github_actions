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

Releases are driven by [Conventional Commits](https://www.conventionalcommits.org/). The [self-release workflow](../README.md#release-flow) advances after CI, Security, and Docker pass for the exact `main` commit.

1. Review and merge the version-bump PR for `Cargo.toml` and `Cargo.lock`. Chore- and docs-only changes do not normally warrant a bump.
2. Review and merge the PR pinning both container action Dockerfiles to the signed runtime built for that version.
3. After validation on `main`, automation creates the `v*` tag and GitHub Release, updates the major alias, and promotes the verified runtime digest.
4. Automation explicitly dispatches `release.yml` on the new tag to build and publish the release assets.

A manual `auto-release.yml` dispatch with a `version_bump` choice can propose a release when no commit qualifies. Both PRs use the usual review rules.
If commits added during review require a larger pending version, the tag phase
stops. Dispatch the bump phase to propose the corrected version, then repeat
the normal review steps.

## Manual Release

Use this when the automated flow is insufficient (e.g., pre-release versions, hotfixes from a release branch).

### Pre-flight

1. Ensure `main` is green:
   ```bash
   make ci
   ```
2. Update `CHANGELOG.md` — move items from `[Unreleased]` to a new version header.
3. Bump the version in `Cargo.toml`.
4. Commit on a feature branch and merge through the usual owner review:
   ```bash
   git add Cargo.toml docs/CHANGELOG.md
   git commit -m "chore: release v1.2.3"
   ```
5. Tag the reviewed release commit:
   ```bash
   git tag v1.2.3
   git push origin refs/tags/v1.2.3
   ```

### What Happens Next

The `v*` tag triggers `release.yml`:

| Step | Artifact |
|------|----------|
| Cross-compile | Linux x86_64, Linux aarch64, macOS universal, Windows x86_64 |
| Package | `.tar.gz` (Unix) and `.zip` (Windows) with binary + LICENSE + README |
| Publish | crates.io through [trusted publishing](https://crates.io/docs/trusted-publishing) (OIDC); a publish failure fails the run |
| GitHub Release | Checksums + packaged assets attached |

The `docker.yml` workflow also triggers on the tag, producing:

| Step | Artifact |
|------|----------|
| Build | Multi-arch Docker image |
| Scan | Trivy vulnerability scan |
| Sign | Cosign image signature |
| SBOM | CycloneDX image SBOM |
| Push | `ghcr.io/threatflux/<image>:<tag>`; also `docker.io/<namespace>/<image>:<tag>`, signed the same way, only when Docker Hub publishing is turned on ([FAQ](FAQ.md#how-do-i-publish-to-docker-hub)) |

### Required Permissions

| Credential | Holder | Purpose |
|------------|--------|---------|
| `GITHUB_TOKEN` | Automatic | Release assets, GHCR push |
| `DOCKERHUB_USERNAME`, `DOCKERHUB_TOKEN` (optional) | Docker Hub namespace owners | Docker Hub login and push, used only when the `RUST_TEMPLATE_PUBLISH_DOCKERHUB` variable is `true`. The variable is unset by default, so nothing is pushed to Docker Hub |
| crates.io trusted publisher (owner `ThreatFlux`, repository `github_actions`, workflow `release.yml`, environment `crates-io`) | crates.io crate owners | The publish job exchanges its OIDC token (`id-token: write`) for a short-lived crates.io token through `rust-lang/crates-io-auth-action`; no registry token secret exists |

The `crates-io` environment only admits `v*` tags, so a real `release.yml` run must be dispatched on the tag (`gh workflow run release.yml --ref vX.Y.Z -f version=X.Y.Z`), which is what `auto-release.yml` does. That restriction is a repository setting, not a file, so a repository generated from this template has to create the environment itself; see [FAQ: How do I set up crates.io trusted publishing?](FAQ.md#how-do-i-set-up-cratesio-trusted-publishing).

### Rehearsal

`gh workflow run release.yml -f version=<Cargo.toml version> -f source_ref=main -f dry_run=true` builds every target, generates the SBOM, and runs `cargo publish --dry-run`. It creates no tag or GitHub Release and attaches no release assets (the build and SBOM workflow artifacts are still kept for 5 days for inspection), skips the `crates-io` environment, and never authenticates to crates.io.

### Rollback

If a release is defective:

1. Delete the GitHub Release (draft state or full delete).
2. Delete the Git tag: `git push --delete origin v1.2.3`
3. Yank from crates.io if published: `cargo yank --version 1.2.3`
4. Fix, then re-release with the next patch version.
