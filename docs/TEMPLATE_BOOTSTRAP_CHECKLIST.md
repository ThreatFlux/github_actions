# Template Bootstrap Checklist

Run this checklist immediately after generating a new repository from the template.

## Required

1. Replace all placeholders:
   - `PROJECT_NAME`
   - `PROJECT_DESCRIPTION`
   - `YOUR_USERNAME`
   - `PROJECT_REPOSITORY`
   - `TEMPLATE_GITHUB_OWNER`
2. Replace `README.md` with `README_TEMPLATE.md`, then remove `README_TEMPLATE.md`.
3. Update `.github/CODEOWNERS`.
4. Update `Cargo.toml`, package metadata, and any inherited org-specific defaults.
5. Update `SECURITY.md` advisory links if the repository is not under ThreatFlux.
6. Run `make template-check`.

## crates.io Publishing

Skip this section if the project does not publish crates; set the repository variable `RUST_TEMPLATE_PUBLISH_CRATES` to `false` instead.

Generated repositories copy files, not settings, so `release.yml`'s publishing prerequisites must be created by hand before the first release. Full steps are in [FAQ: How do I set up crates.io trusted publishing?](FAQ.md#how-do-i-set-up-cratesio-trusted-publishing):

1. Create the `crates-io` environment (**Settings → Environments**) with deployment branches and tags set to **Selected branches and tags** and a single tag rule `v*`. GitHub would otherwise create the environment on first use with no protection, so a run from any branch could obtain a token crates.io accepts.
2. Publish each new crate's first version by hand, because crates.io cannot add a trusted publisher to a crate that does not exist yet. First run `cargo publish --dry-run --locked` with no token set. It builds the crate, which runs build scripts that could otherwise read the token. Then set a short-lived crates.io token, limited to the `publish-new` scope and that crate's name, as `CARGO_REGISTRY_TOKEN` in that one shell (not `cargo login`) and run `cargo publish --locked --no-verify`, which compiles nothing. Delete the token afterwards.
3. Add a trusted publisher to each crate on crates.io: your owner and repository, workflow `release.yml`, environment `crates-io`.
4. After the first release has published through it, turn on **Require trusted publishing** for each crate.

Keep `RUST_TEMPLATE_PUBLISH_CRATES=false` until steps 1 to 3 are done, so releases do not fail on the publish job.

## Single-Crate Projects

1. Confirm `BINARY_NAME` in `Makefile`.
2. Confirm release artifacts match the intended binary.
3. Confirm the Docker image starts correctly with `make docker-build`.

## Workspace Projects

Set these repository variables or Makefile overrides:

- `RUST_TEMPLATE_BINARY_NAME`
- `RUST_TEMPLATE_BINARY_PACKAGE`
- `RUST_TEMPLATE_SBOM_MANIFEST_PATH`
- `RUST_TEMPLATE_PUBLISH_PACKAGES`
- `RUST_TEMPLATE_PUBLISH_CRATES` (repository variable only)

Recommended values:

- `RUST_TEMPLATE_BINARY_NAME`: the CLI binary to package
- `RUST_TEMPLATE_BINARY_PACKAGE`: the package that owns that binary
- `RUST_TEMPLATE_SBOM_MANIFEST_PATH`: the manifest used for SBOM generation
- `RUST_TEMPLATE_PUBLISH_PACKAGES`: publish order, space separated
- `RUST_TEMPLATE_PUBLISH_CRATES` (repository variable, read by `release.yml`): set to `false` for projects that do not publish to crates.io; unset publishes through crates.io trusted publishing

Runner defaults:

- CI, security, docker, auto-release, and release workflows use GitHub-hosted runners out of the box.
- Only set runner repository variables if you need custom labels:
- `RUST_TEMPLATE_RUNNER_UBUNTU`
- `RUST_TEMPLATE_RUNNER_MACOS`
- `RUST_TEMPLATE_RUNNER_WINDOWS`
- `RUST_TEMPLATE_RUNNER_MACOS_ARM64`
- `RUST_TEMPLATE_RUNNER_MACOS_X64`

## Validation

Run locally:

```bash
make dev-setup
make template-check
make ci
make docker-build
```
