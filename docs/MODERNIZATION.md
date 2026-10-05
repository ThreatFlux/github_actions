# Stable modernization — October 5, 2026

Development, Docker builds and stable CI now use [Rust 1.99.0](https://blog.rust-lang.org/2026/10/01/Rust-1.99.0/).
The package, Clippy and Makefile MSRV stay at **1.97.1**. The MSRV job selects
that compiler explicitly, and the beta/nightly jobs select their own channels
even when the repository toolchain file pins development Rust. Each Rust job
prints its actual compiler and Cargo versions.

## Dependency evidence

The eleven direct registry dependencies resolve to the current non-yanked stable
versions from their [crates.io API metadata](https://crates.io/data-access).
There are no direct dependency holds or prereleases. Requirements for Clap,
Reqwest and `toml_edit` advance; the committed lockfile refreshes compatible
transitive dependencies.

| Crate | Stable version |
| --- | --- |
| anyhow | 1.0.104 |
| clap | 4.6.7 |
| regex | 1.13.1 |
| reqwest | 0.13.5 |
| semver | 1.0.28 |
| serde | 1.0.229 |
| toml_edit | 0.25.15+spec-1.1.0 |
| urlencoding | 2.1.3 |
| walkdir | 2.5.0 |
| mockito | 1.7.2 |
| tempfile | 3.27.0 |

Package version **0.7.5**, public Rust APIs, edition, features, both action input
and output schemas, and every reusable `workflow_call` input, secret and output
stay unchanged. Five test-only emptiness assertions use equivalent typed array
equality for Rust 1.99 Clippy; runtime Rust behavior is unchanged. The published 0.7.5 runtime image pins remain under the existing
two-phase release mechanism. No release or package publication is part of this
change.

## Workflows, tools and containers

All six workflows were checked against the current upstream stable releases and
their native action input definitions. All 73 remote action references remain
immutable full commit SHAs. Updated actions include Rust toolchain, Taiki
installer, Buildx, QEMU, Docker build/push, Anchore SBOM, CodeQL SARIF upload and
TruffleHog. The latter uses action 3.98.0 and the verified **plain `3.98.0` Docker
tag**. Existing current Checkout, Cache, upload/download artifact, GitHub App
token, Scorecard, login, metadata and Cosign installer pins are retained.

Tool versions are fixed at cargo-audit 0.22.2, cargo-deny 0.20.2, cargo-hack
0.6.45, cargo-llvm-cov 0.9.1, cargo-cyclonedx 0.5.9, Cross 0.2.5, pre-commit
4.6.2, Gitleaks 8.30.1, Trivy 0.75.0 and Cosign 3.1.3. The development installer
propagates tool installation failures. Gitleaks downloads retain checksum
verification.

Rust 1.99.0 Bookworm and Debian Bookworm slim bases are pinned to the verified
multi-platform manifest digests. Builds retain the existing non-root runtime
user, Bookworm distribution and embedded CycloneDX 1.5 SBOM. Locked builds and
feature powersets include all targets; benchmark, build and scanner execution
failures propagate. Required image, audit and SBOM artifacts fail on missing
files. Trivy's finding policy remains informational (`exit-code: 0`); scanner
execution and invalid/missing native SARIF fail the job.

GitHub [introduced `job.workflow_repository` and `job.workflow_sha` in September
2026](https://github.blog/changelog/2026-09-03-github-actions-early-september-2026-updates/).
Latest stable actionlint 1.7.12 predates those properties. Its two exact property
diagnostics are scoped to the existing reusable workflow and job object type.
Negative fixtures prove unrelated expressions and other workflow paths still
fail; an independent upstream schema check rejects unknown Checkout inputs.

## Release compatibility

This repository's release workflow now requires its `source_ref` to identify a
branch, tag or full commit SHA within fetched `origin/main` history, and passes
the vetted immutable commit to build, SBOM, assets and publish jobs. Historical
main releases remain valid. An unmerged branch must enter main before this
workflow releases it. Options, revision expressions, pull-request namespaces
and ambiguous branch/tag names fail validation. The helper runs from the trusted
workflow checkout; its Git commands clear hook-local repository/config state.

Checkout credentials remain disabled. Manual annotated-tag creation uses the
existing authenticated GitHub API capability, then creates the tag reference at
that returned tag object. The public maintainer `base-branch` and reusable
release caller contracts still support their existing repository/default branch
selection. The release owner, required workflow gate and two-phase image/tag
handoff are unchanged.

## Validation

The required gate is `make all`, followed by `make msrv test-features-full sbom
lint-strict`. Tests use local HTTP mocks with GitHub mutation tokens unset.
Local evidence includes all 137 Rust tests with zero ignored tests, five Git
lineage/reference/isolation regressions, strict Clippy/rustdoc, native LCOV,
benchmark compilation, locked feature powersets, actual MSRV compilation,
native audit and SBOM reports, actionlint/ShellCheck and yamllint.

Container validation executes help and version as UID 1000 without network
access, inspects the embedded native CycloneDX report, and runs an offline
policy fixture with the rebuilt maintainer. The preserved published container
action also runs offline pin and policy fixtures through its `INPUT_*` wiring.
Live release/publication operations are not executed during validation. Hosted
checks must be verified on the final PR commit.
