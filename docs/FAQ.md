# Frequently Asked Questions

<!--
  FAQ.md — What makes this document good:

  A FAQ reduces repeated support questions by answering them in a searchable,
  scannable flat file. It complements the README (which should stay concise)
  and the full docs (which are organized by topic, not by question).

  Best practices:
  - Use actual questions from issues, discussions, or support channels.
  - Write each answer as a self-contained block — readers jump to one Q, not read linearly.
  - Keep answers short (3-5 sentences max). Link to docs for depth.
  - Group questions by theme if the FAQ exceeds ~15 entries.
  - Remove questions that become obsolete after breaking changes.
  - Use a flat H3 structure so GitHub's TOC auto-generates a clickable list.

  Standard name: FAQ.md (root or docs/)
  When to include: Any project that receives recurring questions in issues.
-->

### How do I use this template?

```bash
gh repo create my-project --template ThreatFlux/rust-cicd-template
cd my-project
```

Then follow the [Bootstrap Checklist](TEMPLATE_BOOTSTRAP_CHECKLIST.md) to replace placeholders, swap the README, and validate with `make template-check`.

### Can I use this for a library crate instead of a binary?

Yes. Remove the `[[bin]]` section from `Cargo.toml`, add a `[lib]` section, and delete or adapt the Dockerfile (libraries typically don't ship container images). The CI and security workflows work identically for libraries.

### How do I add a new crate to a workspace?

1. Create the crate directory: `cargo new crates/my-crate --lib`
2. Add it to the root `Cargo.toml` workspace members list.
3. Set `RUST_TEMPLATE_PUBLISH_PACKAGES` to include the new crate in publish order.
4. Run `make ci` to verify everything links.

### Why does `make template-check` fail?

It means unresolved placeholders still exist in your repo. Run:

```bash
grep -rn "PROJECT_NAME\|PROJECT_DESCRIPTION\|REPLACE_WITH\|YOUR_USERNAME\|TEMPLATE_GITHUB_OWNER" .
```

Replace every match, then re-run `make template-check`.

### How do I change the MSRV?

The minimum supported Rust version is declared in nine places. Update all of them:

- `Cargo.toml` → `rust-version`
- `rust-toolchain.toml` → `channel`
- `Makefile` → `RUST_MSRV`
- `clippy.toml` → `msrv`
- `.github/workflows/ci.yml` → MSRV job matrix
- `.github/workflows/release.yml` → build toolchain
- `.github/workflows/security.yml` → toolchain pin
- `.github/workflows/auto-release.yml` → release automation toolchain
- `Dockerfile` → `FROM rust:` tag

See the [Configuration Reference](../README.md#configuration-reference) for details.

### How do I skip crates.io publishing?

Set the repository variable `RUST_TEMPLATE_PUBLISH_CRATES` to `false`. The release workflow then skips its "Publish to crates.io" job.

Otherwise `release.yml` publishes every stable release through [crates.io trusted publishing](https://crates.io/docs/trusted-publishing), and a publish failure fails the release. No registry token secret is used. For each crate, add a trusted publisher on crates.io with your GitHub owner, repository, workflow `release.yml`, and environment `crates-io`. crates.io only accepts a trusted publisher for a crate that already exists, so publish a new crate's first version by hand. Prerelease versions (`X.Y.Z-...`) are never published.

### How do I use custom CI runners?

Set the appropriate repository variable. For example, to use a self-hosted Ubuntu runner:

1. Go to **Settings → Variables → Actions** in your GitHub repo.
2. Create `RUST_TEMPLATE_RUNNER_UBUNTU` with your runner label (e.g., `self-hosted`).

The workflows read these variables at runtime and fall back to GitHub-hosted runners if unset.

### How do I disable a workflow I don't need?

Delete the workflow file from `.github/workflows/`. If you remove `auto-release.yml`, you'll need to tag releases manually — see [RELEASING.md](RELEASING.md).

### The Docker build fails — what's wrong?

Common causes:

1. **Binary name mismatch** — ensure `BINARY_NAME` in the Makefile matches the `[[bin]]` name in `Cargo.toml`.
2. **Missing system dependencies** — if your crate depends on system libraries (e.g., OpenSSL), add them to the Dockerfile's build stage. The runtime stage is distroless (glibc, libgcc, libssl, CA certificates; no shell or package manager), so anything else the binary needs at runtime has to be linked statically or copied in from the build stage.
3. **Workspace path issues** — for workspaces, set `BINARY_PACKAGE` to the crate that owns the binary.

### How do I add code coverage badges?

1. Enable Codecov or Coveralls for your repository.
2. Add the secret (`CODECOV_TOKEN`) to your repo.
3. Add the badge to your README:
   ```markdown
   [![codecov](https://codecov.io/gh/ThreatFlux/PROJECT_NAME/branch/main/graph/badge.svg)](https://codecov.io/gh/ThreatFlux/PROJECT_NAME)
   ```

The `make coverage` target already generates LCOV output compatible with both services.
