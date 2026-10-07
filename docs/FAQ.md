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

Set the repository variable `RUST_TEMPLATE_PUBLISH_CRATES` to `false`. The release workflow then skips its crates.io jobs.

Otherwise `release.yml` publishes every stable release through [crates.io trusted publishing](https://crates.io/docs/trusted-publishing), and a publish failure fails the release. No registry token secret is used. Prerelease versions (`X.Y.Z-...`) are never published. Set publishing up as described in the next answer before the first release.

### How do I set up crates.io trusted publishing?

A generated repository copies this template's files, not its settings, so do all four steps once per repository (and step 2 onward per crate):

1. **Create the `crates-io` environment and limit it to release tags.** The publish job runs in the `crates-io` environment, and the trusted publisher you add in step 3 only accepts OIDC tokens issued for that environment. GitHub creates an environment that a workflow references on first use, but without any protection rules, so create it first. In **Settings → Environments → New environment**, name it `crates-io`. Under **Deployment branches and tags**, choose **Selected branches and tags** and add a single **tag** rule `v*`, with no branch rule. Or, with `gh`:

   ```bash
   gh api -X PUT repos/OWNER/REPO/environments/crates-io \
     -F 'deployment_branch_policy[protected_branches]=false' \
     -F 'deployment_branch_policy[custom_branch_policies]=true'
   gh api -X POST repos/OWNER/REPO/environments/crates-io/deployment-branch-policies \
     -f name='v*' -f type=tag
   ```

   Only workflow runs on a `v*` tag can then enter the environment. `auto-release.yml` dispatches `release.yml` on the new tag, so releases keep working, while a run started from a branch cannot obtain a token crates.io accepts.
2. **Publish each new crate's first version by hand.** crates.io accepts a trusted publisher only for a crate that already exists, so trusted publishing cannot create a crate. On crates.io, create an API token with the `publish-new` scope, limited to that crate's exact name and a short expiry. From a clean checkout of the commit to release, hand the token to Cargo for this one shell only (not `cargo login`, which saves it to `~/.cargo/credentials.toml`), publish, and forget it:

   ```bash
   read -rs CARGO_REGISTRY_TOKEN && export CARGO_REGISTRY_TOKEN   # paste the token; it is not echoed or saved in shell history
   cargo publish --locked                                        # workspace: cargo publish --locked -p <crate>, in dependency order
   unset CARGO_REGISTRY_TOKEN
   ```

   Then delete the token on crates.io. Never store it as a GitHub secret. `release.yml` skips a version crates.io already serves, so publishing the current version by hand does not make the next release fail.
3. **Add the trusted publisher.** In the crate's settings on crates.io, add a GitHub trusted publisher with your owner, your repository, workflow `release.yml`, and environment `crates-io`. The environment must match, or crates.io refuses the token exchange.
4. **Require trusted publishing.** Once a release has published through the trusted publisher, turn on **Require trusted publishing** in the crate's crates.io settings. API tokens can then no longer publish it.

Until steps 1 to 3 are done for every crate, set `RUST_TEMPLATE_PUBLISH_CRATES=false` so releases do not fail on the publish job.

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
2. **Missing system dependencies** — if your crate depends on system libraries (e.g., OpenSSL), add them to the Dockerfile's build stage.
3. **Workspace path issues** — for workspaces, set `BINARY_PACKAGE` to the crate that owns the binary.

### How do I add code coverage badges?

1. Enable Codecov or Coveralls for your repository.
2. Add the secret (`CODECOV_TOKEN`) to your repo.
3. Add the badge to your README:
   ```markdown
   [![codecov](https://codecov.io/gh/ThreatFlux/PROJECT_NAME/branch/main/graph/badge.svg)](https://codecov.io/gh/ThreatFlux/PROJECT_NAME)
   ```

The `make coverage` target already generates LCOV output compatible with both services.
