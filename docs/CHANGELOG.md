# Changelog

<!--
  CHANGELOG.md — What makes this document good:

  A changelog communicates what changed, when, and why — for users deciding
  whether to upgrade and for maintainers tracking regression sources.

  Best practices:
  - Follow https://keepachangelog.com/en/1.1.0/ format.
  - Group entries under: Added, Changed, Deprecated, Removed, Fixed, Security.
  - Use past tense ("Added X") not imperative ("Add X").
  - Link each version header to the GitHub compare or release URL.
  - Never delete entries — changelogs are append-only history.
  - Include the date in YYYY-MM-DD format for every release.
  - For pre-1.0 projects, note breaking changes explicitly.
  - For monorepos / workspaces, consider per-crate changelogs.
  - Automate with tools like git-cliff when commit discipline is strong.

  Standard name: CHANGELOG.md (root)
  When to include: Every project that ships versioned releases.
-->

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `release/` sub-action (`ThreatFlux/github_actions/release@<ref>`) packaging the release command as a prebuilt-image Docker action for any Cargo repository
- `reusable-auto-release.yml` callable workflow wrapping checkout plus the release action
- Moving `v0` major alias tag maintained by the release automation

### Changed

- `auto-release.yml` now dogfoods the release action instead of the bash/python release steps

### Fixed

- Tag pipelines (`release.yml`, `docker.yml`) never ran for pushed tags because tags created with `GITHUB_TOKEN` do not trigger workflows; `auto-release.yml` now dispatches them explicitly on the new tag
- Container signing now also runs for `v*` tag builds instead of only `main`

## [0.3.0] - 2026-08-07

### Added

- `release` CLI command: conventional-commit-driven version bumps, Cargo manifest/lockfile rewriting, and API-driven release commit, tag, and GitHub Release creation

## [0.2.3] - 2026-08-01

### Added

- ARCHITECTURE.md with component map and design decision rationale
- CHANGELOG.md following Keep a Changelog format
- RELEASING.md with maintainer release runbook
- FAQ.md covering common setup and customization questions
- Expanded README_STANDARDS.md with comprehensive style guide
- Mermaid CI/CD pipeline diagram in README.md
- Table of contents and back-to-top navigation in all READMEs
- Centered header blocks with badge rows and quick navigation links

### Changed

- Reframed project identity from "CI/CD Template" to "Rust Project Template"
- Reorganized README.md configuration section into structured tables
- Updated README_TEMPLATE.md to inherit all structural best practices
- Updated Cargo.toml description and keywords

### Maintenance

- Refreshed Rust dependencies and Cargo.lock to the latest compatible stable releases
- Updated Docker and CI toolchains to Rust 1.97.1
- Updated pinned GitHub Actions and security tooling

## [0.5.0] - 2025-03-24

### Added

- Initial public template release
- GitHub Actions workflows: ci.yml, security.yml, release.yml, auto-release.yml, docker.yml
- Makefile with full build, test, lint, security, and release targets
- Dockerfile with multi-stage build, Trivy scan, Cosign signing
- Template placeholder validation via `make template-check`
- Repository governance files: CODEOWNERS, issue templates, PR template
- CONTRIBUTING.md, SECURITY.md, CODE_OF_CONDUCT.md
- README_TEMPLATE.md starter for generated projects
- Bootstrap checklist and README standards documentation
- Rust 2024 edition default with 1.96.0 MSRV baseline

[0.2.3]: https://github.com/ThreatFlux/github_actions/compare/v0.2.2...HEAD
[0.5.0]: https://github.com/ThreatFlux/rust-cicd-template/releases/tag/v0.5.0
