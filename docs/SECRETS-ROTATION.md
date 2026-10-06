# Secrets Rotation

<!--
  SECRETS-ROTATION.md: the inventory and runbook for the ThreatFlux
  organization's GitHub Actions secrets. It never contains a secret value.
  Keep the inventory in step with `gh api /orgs/ThreatFlux/actions/secrets`
  whenever a secret is added, rotated, or deleted.
-->

ThreatFlux keeps CI credentials as **GitHub organization Actions secrets**. Org owners hold them, and a secret is visible only to the repositories its visibility allows. Wherever a platform supports it, a short-lived OIDC credential replaces a stored secret:

| Purpose | Credential | Stored secret needed |
| --- | --- | --- |
| crates.io publishing | [Trusted publishing](https://crates.io/docs/trusted-publishing): `rust-lang/crates-io-auth-action` exchanges the job's OIDC token for a short-lived crates.io token (job needs `id-token: write` and the `crates-io` environment) | None |
| GHCR images | The workflow's `GITHUB_TOKEN` with `packages: write` | None |
| Container signing | Keyless cosign (Sigstore Fulcio certificate from the job's OIDC token) | None |
| Cross-repository automation (release PRs, tags that trigger workflows) | `threatflux-automation` GitHub App installation token from `actions/create-github-app-token` | The App private key, one secret |

The [Automation App Health](../.github/workflows/automation-app-health.yml) workflow checks all of this every week; see [Health check](#health-check).

## Inventory

Names and dates come from `gh api /orgs/ThreatFlux/actions/secrets` (as of 2026-10-06). The "Used by" column lists the default-branch workflows that reference each secret across every ThreatFlux repository. A secret with visibility `all` can be read by any repository, so this column shows usage, not reach.

| Secret | Visibility | Last updated | Purpose (issuer) | Used by | Cadence | Status |
| --- | --- | --- | --- | --- | --- | --- |
| `TF_AUTOMATION_APP_PRIVATE_KEY` | selected (7 repos) | 2026-10-06 | Private key of the `threatflux-automation` GitHub App (App ID in the `TF_AUTOMATION_APP_ID` org variable) | The 7 installation repositories: `github_actions`, `ollama_rust_sdk`, `rust-cicd-template`, `threatflux-atlassian`, `threatflux-package-security`, `threatflux-unifi-sdk`, `vertex_rust_sdk` (through `reusable-auto-release.yml` and `automation-app-health.yml`) | 90 days | Active |
| `CARGO_REGISTRY_TOKEN` | all | 2025-08-14 | crates.io API token | `release.yml` in FluxPrompt, anthropic_rust_sdk, gguf, github_actions, ollama_rust_sdk, openai_rust_sdk, rust-cicd-template, threatflux-atlassian, threatflux-binary-analysis, threatflux-cache, threatflux-hashing, threatflux-package-security, threatflux-string-analysis, threatflux-threat-detection, threatflux-unifi-sdk, vertex_rust_sdk, virustotal-rs | n/a | **Obsolete, delete.** crates.io rejects it (403). Replaced by trusted publishing |
| `GIT_TOKEN` | all | 2025-03-31 | Classic personal access token | YaraFlux `update-actions.yml`; lifeflux `auto-release.yml`, `ci.yml`, `quality.yml`, `security.yml` (fetches private dependencies) | n/a | **Obsolete, delete.** GitHub rejects it (401), so these workflows already fail where they need it |
| `ANTHROPIC_API_KEY` | private | 2025-06-19 | Anthropic API key (Anthropic console) | anthropic_rust_sdk `ci.yml` (that repository is public, so `private` visibility already hides the key from it) | 180 days | Rotate; review whether it is still needed |
| `CODACY_API_TOKEN` | all | 2025-08-20 | Codacy API token (Codacy account settings) | No workflow references it | 180 days | Unused; confirm with the Codacy owner, then delete |
| `CODACY_ORGANIZATION_PROVIDER` | all | 2025-08-20 | Codacy organization provider (`gh`), not a credential | No workflow references it | n/a | Unused; delete with `CODACY_API_TOKEN` |
| `CODACY_USERNAME` | all | 2025-08-20 | Codacy organization name, not a credential | No workflow references it | n/a | Unused; delete with `CODACY_API_TOKEN` |
| `CODECOV_TOKEN` | all | 2025-03-10 | Codecov upload token (Codecov organization settings) | `ci.yml` in FluxEncrypt, FluxPrompt, YaraFlux, anthropic_rust_sdk, file-scanner, ollama_rust_sdk, openai_rust_sdk, threatflux-atlassian; lifeflux and openai_rust_sdk `quality.yml` | 180 days | Rotate |
| `DOCKERHUB_TOKEN` | all | 2025-02-01 | Docker Hub access token for the `threatflux` namespace (Docker Hub account settings) | YaraFlux `publish-release.yml`; `docker.yml` in github_actions, lifeflux, openai_rust_sdk, rust-cicd-template, threatflux-unifi-sdk (template repositories only log in when `RUST_TEMPLATE_PUBLISH_DOCKERHUB=true`) | 180 days | Rotate |
| `DOCKERHUB_USERNAME` | all | 2025-02-01 | Docker Hub account name, not a credential | Same as `DOCKERHUB_TOKEN` | With the token | Keep with the token |
| `GITLEAKS_LICENSE` | all | 2025-08-21 | Gitleaks action license key | `security.yml` in lifeflux, openai_rust_sdk | On renewal, at most 180 days | Rotate |
| `PUB_DEV_CREDENTIALS` | all | 2025-03-10 | pub.dev publishing credentials | No workflow references it | n/a | Unused; delete (pub.dev also supports OIDC publishing from GitHub Actions) |
| `SAFETY_API_KEY` | all | 2025-02-13 | Safety CLI API key | No workflow references it | n/a | Unused; delete |
| `THREATFLUX_REGISTRY_PASSWORD` | all | 2026-01-04 | Password for the ThreatFlux container registry | No workflow references it | 180 days | Unused in Actions; confirm with its owner, then delete or move to `selected` |

Repository-level secrets that workflows also reference (not part of the org audit): `RELEASE_TOKEN` (optional override in 11 `auto-release.yml` files; superseded by the automation App), `CRATES_IO_TOKEN` (optional override in 5 `release.yml` files; superseded by trusted publishing), `DEPS_APP_PRIVATE_KEY` (ollama_rust_sdk dependency App), and project-specific keys (`SEMGREP_APP_TOKEN`, `SNYK_TOKEN`, `OPENAI_API_KEY`, termflux Apple signing secrets).

To refresh the "Used by" column:

```bash
gh api /orgs/ThreatFlux/actions/secrets --paginate --jq '.secrets[] | [.name, .visibility, .updated_at] | @tsv'
gh search code --owner ThreatFlux 'secrets.NAME' --json repository,path --jq '.[] | "\(.repository.nameWithOwner) \(.path)"'
```

## Deleting the obsolete secrets

`CARGO_REGISTRY_TOKEN` and `GIT_TOKEN` no longer authenticate, so deleting them cannot break anything that works today. Delete each after its verification:

1. **`CARGO_REGISTRY_TOKEN`**: for every repository that publishes crates, confirm its `release.yml` authenticates through `rust-lang/crates-io-auth-action` and that the latest release's "Publish to crates.io" job shows the OIDC token exchange. Then remove the remaining `secrets.CARGO_REGISTRY_TOKEN` and `secrets.CRATES_IO_TOKEN` references, turn on **Require trusted publishing** for each crate on crates.io, and run `gh secret delete CARGO_REGISTRY_TOKEN --org ThreatFlux`.
2. **`GIT_TOKEN`**: replace the remaining references (lifeflux private-dependency fetches, YaraFlux `update-actions.yml`) with `GITHUB_TOKEN` or a GitHub App installation token, then run `gh secret delete GIT_TOKEN --org ThreatFlux`. Revoke the personal access token too, if it still exists in its owner's developer settings.

Until both are gone, every Automation App Health run lists them as findings.

## Rotation procedures

Rotate on the cadence above, immediately if a value may have leaked, and whenever the person who issued a token leaves. Each rotation follows the same pattern: issue the new credential, store it with `gh secret set NAME --org ThreatFlux --visibility <same as today>` (for `selected`, keep the same `--repos`), run a workflow that uses it, and only then revoke the old credential at its issuer.

| Secret | Issue a new value at | Prove it works |
| --- | --- | --- |
| `TF_AUTOMATION_APP_PRIVATE_KEY` | The App's settings page; follow the [App key runbook](#app-key-runbook) | The script dispatches Automation App Health |
| `CODECOV_TOKEN` | Codecov, organization settings, global upload token | Rerun the CI workflow of a repository that uploads coverage |
| `DOCKERHUB_TOKEN` | Docker Hub, Account settings, Personal access tokens (read and write, scoped to `threatflux`) | Dispatch a `docker.yml` that publishes to Docker Hub |
| `GITLEAKS_LICENSE` | The Gitleaks license portal | Rerun `security.yml` in lifeflux or openai_rust_sdk |
| `ANTHROPIC_API_KEY` | The Anthropic console, API keys | Rerun the anthropic_rust_sdk tests that use it |
| `THREATFLUX_REGISTRY_PASSWORD` | The ThreatFlux registry's user settings | Log in with `docker login` before deleting the old password |

Secrets that are only names (`*_USERNAME`, `CODACY_ORGANIZATION_PROVIDER`) need no rotation.

## App key runbook

The `threatflux-automation` App (installation on the ThreatFlux organization) authenticates release automation in the 7 repositories above. GitHub has no API that creates App private keys, so a person generates the key and [`scripts/rotate-automation-app-key.sh`](../scripts/rotate-automation-app-key.sh) does everything else.

Prerequisites: `gh` signed in as an organization owner with the `admin:org` scope (`gh auth refresh -s admin:org`), plus `jq`, `curl`, and `openssl`.

1. Open the App's settings: ThreatFlux organization settings, Developer settings, GitHub Apps, `threatflux-automation` (`https://github.com/organizations/ThreatFlux/settings/apps/threatflux-automation`). Under **Private keys**, choose **Generate a private key**. The browser downloads `threatflux-automation.<date>.private-key.pem`. An App can hold several keys at once, so the old key keeps working for now.
2. Rehearse: `scripts/rotate-automation-app-key.sh --dry-run ~/Downloads/threatflux-automation.*.private-key.pem`. It checks that the file is an unencrypted RSA key, signs a JWT with it and calls `GET /app` to prove the key belongs to App `TF_AUTOMATION_APP_ID`, and reads the secret's current repository list. It changes nothing and keeps the file.
3. Rotate: run the same command without `--dry-run`. After a confirmation (`--yes` skips it), the script:
   - replaces `TF_AUTOMATION_APP_PRIVATE_KEY` with `gh secret set --org ThreatFlux --visibility selected --repos <the same list>`, feeding the key on stdin;
   - dispatches `automation-app-health.yml` and waits for it to pass, proving Actions can mint installation tokens with the new key;
   - prints the settings URL and the new key's `SHA256:` fingerprint;
   - removes the local `.pem` (`rm -P` on macOS, `shred -u` on Linux). On SSDs and APFS an overwrite is not guaranteed, so also empty the Downloads folder and the trash.
4. In the App's settings, delete every private key whose fingerprint differs from the one the script printed.
5. Dispatch Automation App Health once more (`gh workflow run automation-app-health.yml -R ThreatFlux/github_actions`). It must still pass, which proves nothing depended on the old key.

If the health run fails, the script stops before step 4 and keeps the `.pem`. The old key still works, so nothing is broken: fix the cause (usually the secret's repository list) and run the script again.

**Suspected compromise:** generate a new key, run the script, delete the compromised key at once, and review the App's recent activity: the organization audit log entries for the App, and the recent tags, releases, and pull requests by `threatflux-automation[bot]` in the installation repositories.

## Health check

[`automation-app-health.yml`](../.github/workflows/automation-app-health.yml) runs every Monday at 07:17 UTC and on demand (`gh workflow run automation-app-health.yml -R ThreatFlux/github_actions`). It mints installation tokens with the stored key, each limited to the permission its check needs:

| Check | Token permission | Failure means |
| --- | --- | --- |
| Mint a token | `metadata: read` | **broken**: the key or App ID is wrong, or the secret is not shared with `github_actions`; the run fails |
| List the installation's repositories and compare them with `EXPECTED_REPOSITORIES` | `metadata: read` | **broken**: a repository was added to or removed from the installation; the run fails |
| List the organization's secrets (names, dates, and visibility only) | `organization_secrets: read` | **attention**: the App private key is older than 90 days, another credential is older than 180 days (name-only secrets such as `DOCKERHUB_USERNAME` are exempt), `GIT_TOKEN` or `CARGO_REGISTRY_TOKEN` still exists, or the key secret is not limited to exactly the installation's repositories |

The secret audit needs the App's organization permission **Secrets: Read-only**. An organization owner grants it in the App's settings (Permissions and events, Organization permissions) and then approves the updated permissions on the installation. Until then the run reports "Organization secret audit skipped" as a finding instead of failing silently.

The workflow has no `pull_request` trigger, because a pull request would run its own copy of a job that holds the App key. Its evaluation logic is unit tested in CI instead.

Findings go to a single issue labelled `secret-rotation` in this repository, created with the workflow's `GITHUB_TOKEN` (`issues: write`). Each run rewrites the issue body with the latest report, reopens the issue if it was closed and something is wrong again, and closes it once a run finds nothing.
