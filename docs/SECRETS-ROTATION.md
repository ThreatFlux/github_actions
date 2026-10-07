# Secrets Rotation

<!--
  SECRETS-ROTATION.md: the inventory and runbook for the ThreatFlux
  organization's GitHub Actions secrets. It never contains a secret value.
  Keep the inventory in step with `gh api /orgs/ThreatFlux/actions/secrets`
  whenever a secret is added, rotated, or deleted.
-->

ThreatFlux keeps CI credentials as **GitHub organization Actions secrets**. Org owners (@wroersma and @vtriple as of 2026-10-07) hold them, and a secret is visible only to the repositories its visibility allows. Wherever a platform supports it, a short-lived OIDC credential replaces a stored secret:

| Purpose | Credential | Stored secret needed |
| --- | --- | --- |
| crates.io publishing | [Trusted publishing](https://crates.io/docs/trusted-publishing): `rust-lang/crates-io-auth-action` exchanges the job's OIDC token for a short-lived crates.io token (job needs `id-token: write` and the `crates-io` environment). All 20 crates published from ThreatFlux repositories have **Require trusted publishing** on, so crates.io refuses API tokens for them | None |
| GHCR images | The workflow's `GITHUB_TOKEN` with `packages: write` | None |
| Container signing | Keyless cosign (Sigstore Fulcio certificate from the job's OIDC token) | None |
| Codecov uploads | `codecov/codecov-action` with `use_oidc: true` (job needs `id-token: write`). Every default-branch workflow that uploads coverage does this, so the old `CODECOV_TOKEN` secret is [being retired](#retiring-git_token-and-codecov_token) | None |
| Cross-repository automation (release PRs, tags that trigger workflows) | `threatflux-automation` GitHub App installation token from `actions/create-github-app-token` | The App private key, one secret |

The [Automation App Health](../.github/workflows/automation-app-health.yml) workflow checks all of this every week; see [Health check](#health-check).

## Inventory

Names and dates come from `gh api /orgs/ThreatFlux/actions/secrets` (as of 2026-10-07). The "Used by" column lists the default-branch workflows that reference each secret across every non-archived ThreatFlux repository on that date. A secret with visibility `all` can be read by any repository, so this column shows usage, not reach. A cadence counts from the "Last updated" date, except for the App key, which counts from its [rotation record](#key-age-tracking).

| Secret | Visibility | Last updated | Purpose (issuer) | Used by | Cadence | Status |
| --- | --- | --- | --- | --- | --- | --- |
| `TF_AUTOMATION_APP_PRIVATE_KEY` | selected (20 repos) | 2026-10-06 | Private key of the `threatflux-automation` GitHub App (App ID in the `TF_AUTOMATION_APP_ID` org variable) | The 20 repositories in [`.github/automation-app-repos.txt`](../.github/automation-app-repos.txt), mostly through their `auto-release.yml` (and `reusable-auto-release.yml`); also `automation-app-health.yml` and `reusable-release-smoke.yml` here, anthropic_rust_sdk `release.yml`, and ollama_rust_sdk `dependencies.yml` | 90 days, counted from its [rotation record](#key-age-tracking) | Active. The record dates the stored key to 2026-10-06, so it is due by 2027-01-04 |
| `GIT_TOKEN` | all | 2025-03-31 | Classic personal access token | Nothing, on any branch. ThreatFlux/lifeflux#24 removed the last references (lifeflux `auto-release.yml`, `ci.yml`, `quality.yml`, `security.yml`) on 2026-10-07. YaraFlux, the other former user, is archived | n/a | **Retiring.** GitHub rejects it (401) and no workflow reads it, so an org owner can delete it now. See [Retiring `GIT_TOKEN` and `CODECOV_TOKEN`](#retiring-git_token-and-codecov_token) |
| `CODECOV_TOKEN` | all | 2025-03-10 | Codecov upload token (Codecov organization settings) | No default-branch workflow. The last three moved to Codecov OIDC on 2026-10-07: ollama_rust_sdk and threatflux-atlassian `ci.yml` (ThreatFlux/ollama_rust_sdk#96, ThreatFlux/threatflux-atlassian#112) and lifeflux `quality.yml` (ThreatFlux/lifeflux#24). The other repositories already uploaded with OIDC. Two non-default branches still pass it: threatflux-atlassian `dev` and lifeflux `feat/stable-modernization-20261005` | None: do not rotate | **Retiring.** Delete it once those two branches are updated from their default branch, or removed. See [Retiring `GIT_TOKEN` and `CODECOV_TOKEN`](#retiring-git_token-and-codecov_token) |
| `ANTHROPIC_API_KEY` | private | 2025-06-19 | Anthropic API key (Anthropic console) | anthropic_rust_sdk `ci.yml` (that repository is public, so `private` visibility already hides the key from it) | 180 days | **Rotate**, overdue since 2025-12-16; review whether it is still needed |
| `DOCKERHUB_TOKEN` | all | 2025-02-01 | Docker Hub access token for the `threatflux` namespace (Docker Hub account settings) | `docker.yml` in github_actions, lifeflux, openai_rust_sdk, rust-cicd-template, threatflux-unifi-sdk (template repositories only log in when `RUST_TEMPLATE_PUBLISH_DOCKERHUB=true`) | 180 days | **Rotate**, overdue since 2025-07-31 |
| `DOCKERHUB_USERNAME` | all | 2025-02-01 | Docker Hub account name, not a credential | Same as `DOCKERHUB_TOKEN` | With the token | Keep with the token |
| `GITLEAKS_LICENSE` | all | 2025-08-21 | Gitleaks action license key | `security.yml` in lifeflux, openai_rust_sdk | On renewal, at most 180 days | **Rotate**, overdue since 2026-02-17 |
| `THREATFLUX_REGISTRY_PASSWORD` | all | 2026-01-04 | Password for the ThreatFlux container registry | No workflow references it | 180 days | Unused in Actions. **Rotate** (overdue since 2026-07-03), or confirm with its owner and then delete it or move it to `selected` |

### Deleted secrets

These organization secrets were deleted on 2026-10-07, once nothing used them. The [health check](#health-check) reports them as still deleted, and raises a finding if any of them is created again.

| Secret | Was | Why it went |
| --- | --- | --- |
| `CARGO_REGISTRY_TOKEN` | crates.io API token | crates.io rejected it (403), and no workflow read it: every `release.yml` that publishes crates authenticates through `rust-lang/crates-io-auth-action`. See [crates.io trusted publishing](#cratesio-trusted-publishing) |
| `CODACY_API_TOKEN` | Codacy API token (Codacy account settings) | No workflow referenced it |
| `CODACY_ORGANIZATION_PROVIDER` | Codacy organization provider (`gh`), not a credential | No workflow referenced it; deleted with `CODACY_API_TOKEN` |
| `CODACY_USERNAME` | Codacy organization name, not a credential | No workflow referenced it; deleted with `CODACY_API_TOKEN` |
| `PUB_DEV_CREDENTIALS` | pub.dev publishing credentials | No workflow referenced it. pub.dev supports OIDC publishing from GitHub Actions if a package ever needs it |
| `SAFETY_API_KEY` | Safety CLI API key | No workflow referenced it |

Deleting a GitHub secret does not revoke the credential at its issuer. If the Codacy API token, the pub.dev credentials, or the Safety API key still work, revoke them in the issuer's settings too.

Organization variables that belong to the App (values are not secret, but their repository access is audited like the key's):

| Variable | Visibility | Holds | Written by |
| --- | --- | --- | --- |
| `TF_AUTOMATION_APP_ID` | selected (the same 20 repositories) | The App ID | An owner; `scripts/rotate-automation-app-key.sh --sync-repos` re-shares it |
| `TF_AUTOMATION_APP_KEY_FINGERPRINT` | selected (`github_actions`) | `SHA256:` fingerprint of the key in `TF_AUTOMATION_APP_PRIVATE_KEY` | The rotation script; see [Key age tracking](#key-age-tracking) |
| `TF_AUTOMATION_APP_KEY_ROTATED_AT` | selected (`github_actions`) | When that key was stored (UTC) | The rotation script |

Repository-level secrets that workflows also reference (not part of the org audit): `RUST_TEMPLATE_RELEASE_APP_PRIVATE_KEY` (optional App key override in rust-cicd-template `auto-release.yml`), and project-specific keys (`OPENAI_API_KEY` and `SEMGREP_APP_TOKEN` in openai_rust_sdk; `SEMGREP_APP_TOKEN` and `SNYK_TOKEN` in lifeflux; termflux Apple signing and App Store Connect secrets). No default-branch workflow reads `CRATES_IO_TOKEN` or `RELEASE_TOKEN` any more.

To refresh the "Used by" column:

```bash
gh api /orgs/ThreatFlux/actions/secrets --paginate --jq '.secrets[] | [.name, .visibility, .updated_at] | @tsv'
gh search code --owner ThreatFlux 'secrets.NAME' --json repository,path --jq '.[] | "\(.repository.nameWithOwner) \(.path)"'
```

Code search can lag behind recent pushes; for an exact answer, read each repository's `.github/workflows` on its default branch. Before deleting a secret, also check the other branches: [Retiring `GIT_TOKEN` and `CODECOV_TOKEN`](#retiring-git_token-and-codecov_token) shows how.

## crates.io trusted publishing

Every crate published from a ThreatFlux repository publishes through trusted publishing, and has **Require trusted publishing** turned on in its crates.io settings, so no API token can publish it. As of 2026-10-07 that is all 20 of them: `fluxencrypt`, `fluxencrypt-async`, `fluxencrypt-cli`, `fluxprompt`, `gguf-rs-lib`, `github-actions-maintainer`, `ollama_rust_sdk`, `openai_rust_sdk`, `threatflux-anthropic-sdk`, `threatflux-atlassian-cli`, `threatflux-atlassian-sdk`, `threatflux-binary-analysis`, `threatflux-cache`, `threatflux-hashing`, `threatflux-package-security`, `threatflux-string-analysis`, `threatflux-threat-detection`, `threatflux-unifi-sdk`, `threatflux-vertex-rust-sdk`, and `virustotal-rs`. That is why `CARGO_REGISTRY_TOKEN` could be deleted. To check the setting, read each crate's `trustpub_only` flag:

```bash
curl -fsS -A 'ThreatFlux secrets audit' https://crates.io/api/v1/crates/NAME | jq '.crate.trustpub_only'
```

A new crate's first version still has to be published by hand, because crates.io cannot add a trusted publisher to a crate that does not exist yet; [TEMPLATE_BOOTSTRAP_CHECKLIST.md](TEMPLATE_BOOTSTRAP_CHECKLIST.md) describes that publish, which uses a short-lived token that is deleted afterwards. Turn on **Require trusted publishing** as soon as the trusted publisher is added.

## Retiring `GIT_TOKEN` and `CODECOV_TOKEN`

Neither secret should be rotated. Both are being retired. Since ThreatFlux/lifeflux#24 merged on 2026-10-07, no default-branch workflow in any non-archived ThreatFlux repository reads either of them:

1. **`GIT_TOKEN`** no longer authenticates, so deleting it cannot break anything that works today. ThreatFlux/lifeflux#24 removed its last references, in lifeflux `auto-release.yml`, `ci.yml`, `quality.yml` and `security.yml`; lifeflux's dependencies come from crates.io and public git repositories, so those workflows need no token. No workflow on any branch reads it, so an org owner can delete it now: run `gh secret delete GIT_TOKEN --org ThreatFlux`, and revoke the personal access token too, if it still exists in its owner's developer settings.
2. **`CODECOV_TOKEN`** still works, but every default-branch coverage upload now authenticates with OIDC: the coverage job has `id-token: write` and passes `use_oidc: true` to `codecov/codecov-action` instead of `token:`. ThreatFlux/ollama_rust_sdk#96, ThreatFlux/threatflux-atlassian#112 and ThreatFlux/lifeflux#24 moved the last three default-branch workflows on 2026-10-07. A push or pull request on another branch runs that branch's own copy of the workflow, though, and as of 2026-10-07 two branches still pass the token:
   - threatflux-atlassian `dev`: `ci.yml` runs on pushes to `dev` and on pull requests into it. The branch has no commits of its own (it is 53 commits behind `main`, last pushed 2026-03-14), so updating it to `main` or deleting it removes the reference.
   - lifeflux `feat/stable-modernization-20261005`, the head of ThreatFlux/lifeflux#23: `quality.yml` runs for that pull request. The pull request conflicts with `main`; resolve `quality.yml` in favor of `main`'s OIDC upload step.

   Both upload steps set `fail_ci_if_error: false`, so deleting the secret first would not fail CI, but those branches would lose their coverage uploads. Once neither branch passes the token, run `gh secret delete CODECOV_TOKEN --org ThreatFlux`, then regenerate the global upload token in Codecov's organization settings, so the old value stops working.

Before deleting either secret, check every branch, not only default branches (code search only covers default branches). Run this from an empty directory. It stops with an error if a repository cannot be listed, cloned, or searched, so a scan that ends with the "Scanned" line covered every branch:

```bash
bash -euo pipefail <<'SCAN'
repos=$(gh repo list ThreatFlux --no-archived --limit 200 --json name --jq '.[].name')
[ -n "$repos" ] || { echo "gh repo list returned no repositories" >&2; exit 1; }
for repo in $repos; do
  git clone --quiet --bare --depth 1 --no-single-branch "https://github.com/ThreatFlux/$repo.git" "$repo.git"
  status=0
  git -C "$repo.git" grep -n -E 'secrets\.(GIT_TOKEN|CODECOV_TOKEN)|vars\.GIT_TOKEN' \
    $(git -C "$repo.git" for-each-ref --format='%(refname)' refs/heads) -- .github > "$repo.hits" || status=$?
  [ "$status" -le 1 ] || { echo "git grep failed in $repo" >&2; exit 1; }
  sed "s|^|$repo |" "$repo.hits"
done
echo "Scanned every branch of $(wc -w <<<"$repos" | tr -d ' ') repositories; any reference is listed above."
SCAN
```

Until each secret is gone, every Automation App Health run lists it as an obsolete secret. Once deleted, move it from the `--obsolete` entries to the `--deleted` list in [`automation-app-health.yml`](../.github/workflows/automation-app-health.yml), and move its row to [Deleted secrets](#deleted-secrets).

## Rotation procedures

Rotate on the cadence above, immediately if a value may have leaked, and whenever the person who issued a token leaves. Each rotation follows the same pattern: issue the new credential, store it with `gh secret set NAME --org ThreatFlux --visibility <same as today>` (for `selected`, keep the same `--repos`), run a workflow that uses it, and only then revoke the old credential at its issuer.

| Secret | Issue a new value at | Prove it works |
| --- | --- | --- |
| `TF_AUTOMATION_APP_PRIVATE_KEY` | The App's settings page; follow the [App key runbook](#app-key-runbook) | The script dispatches Automation App Health |
| `DOCKERHUB_TOKEN` | Docker Hub, Account settings, Personal access tokens (read and write, scoped to `threatflux`) | Dispatch a `docker.yml` that publishes to Docker Hub |
| `GITLEAKS_LICENSE` | The Gitleaks license portal | Rerun `security.yml` in lifeflux or openai_rust_sdk |
| `ANTHROPIC_API_KEY` | The Anthropic console, API keys | Rerun the anthropic_rust_sdk tests that use it |
| `THREATFLUX_REGISTRY_PASSWORD` | The ThreatFlux registry's user settings | Log in with `docker login` before deleting the old password |

`DOCKERHUB_USERNAME` only holds a name and needs no rotation. `GIT_TOKEN` and `CODECOV_TOKEN` are [being retired](#retiring-git_token-and-codecov_token) instead of rotated.

## App key runbook

The `threatflux-automation` App (installation on the ThreatFlux organization) authenticates release automation in the repositories listed in [`.github/automation-app-repos.txt`](../.github/automation-app-repos.txt), 20 as of 2026-10-06. GitHub has no API that creates App private keys, so a person generates the key and [`scripts/rotate-automation-app-key.sh`](../scripts/rotate-automation-app-key.sh) does everything else.

Prerequisites: `gh` signed in as an organization owner with the `admin:org` scope (`gh auth refresh -s admin:org`), plus `jq`, `curl`, and `openssl`. Run the script from an up-to-date checkout of `main`, because it reads the repository list from it.

1. Open the App's settings: ThreatFlux organization settings, Developer settings, GitHub Apps, `threatflux-automation` (`https://github.com/organizations/ThreatFlux/settings/apps/threatflux-automation`). Under **Private keys**, choose **Generate a private key**. The browser downloads `threatflux-automation.<date>.private-key.pem`. An App can hold several keys at once, so the old key keeps working for now.
2. Rehearse: `scripts/rotate-automation-app-key.sh --dry-run ~/Downloads/threatflux-automation.*.private-key.pem`. It checks that `TF_AUTOMATION_APP_PRIVATE_KEY` and `TF_AUTOMATION_APP_ID` are shared with exactly the listed repositories, that the file is an unencrypted RSA key, signs a JWT with it and calls `GET /app` to prove the key belongs to App `TF_AUTOMATION_APP_ID`, and refuses to continue if any listed repository has a repository secret with the same name (it would override the org secret and keep the old key in use). It changes nothing and keeps the file.
3. Rotate: run the same command without `--dry-run`. After a confirmation (`--yes` skips it), the script:
   - replaces `TF_AUTOMATION_APP_PRIVATE_KEY` with `gh secret set --org ThreatFlux --visibility selected --repos <the listed repositories>`, feeding the key on stdin;
   - records the new key in `TF_AUTOMATION_APP_KEY_ROTATED_AT` and `TF_AUTOMATION_APP_KEY_FINGERPRINT` (see [Key age tracking](#key-age-tracking));
   - dispatches `automation-app-health.yml` and waits for it to pass, proving Actions can mint installation tokens with the new key;
   - prints the settings URL and the new key's `SHA256:` fingerprint;
   - removes the local `.pem` (`rm -P` on macOS, `shred -u` on Linux). On SSDs and APFS an overwrite is not guaranteed, so also empty the Downloads folder and the trash.
4. In the App's settings, delete every private key whose fingerprint differs from the one the script printed.
5. Dispatch Automation App Health once more (`gh workflow run automation-app-health.yml -R ThreatFlux/github_actions`). It must still pass, which proves nothing depended on the old key.

If the health run fails, the script stops before step 4 and keeps the `.pem`. The old key still works, so nothing is broken: fix the cause (usually repository access) and run the script again.

If the secret's or the variable's repository access differs from the list, the script stops before changing anything and names the difference. Either fix the list in a pull request, or rerun with `--sync-repos` to share both with exactly the listed repositories as part of the rotation.

**Who can read the key:** an organization secret reaches every workflow in its selected repositories, on any branch. Anyone who can push a branch to one of the 20 repositories can therefore run a workflow that reads `TF_AUTOMATION_APP_PRIVATE_KEY`, and no trigger or `if:` in a workflow file changes that, because the branch's own copy of the file is what runs. Keep write access to those repositories limited to people trusted with the App. A stronger boundary would hold the key as an environment secret behind a deployment policy that only admits `main`, at the cost of one copy of the secret per repository.

**Suspected compromise:** generate a new key, run the script, delete the compromised key at once, and review the App's recent activity: the organization audit log entries for the App, and the recent tags, releases, and pull requests by `threatflux-automation[bot]` in the listed repositories.

### Changing the repository set

[`.github/automation-app-repos.txt`](../.github/automation-app-repos.txt) is the single list of repositories the App serves. Three settings must match it exactly, and the health check fails when any of them differs in either direction:

- the installation's repository access (App settings, Install App, ThreatFlux, Repository access);
- the selected repositories of the `TF_AUTOMATION_APP_PRIVATE_KEY` organization secret;
- the selected repositories of the `TF_AUTOMATION_APP_ID` organization variable.

To add or remove a repository, change the list in a pull request, then update the three settings. `scripts/rotate-automation-app-key.sh --check-repos` compares the secret and the variable with the list as an organization owner without needing a key; the health check covers the installation.

### Key age tracking

The 90-day key limit needs the date the key was stored. The secret's own `updated_at` is not reliable for that: it also changes when only the secret's repository access changes (as it did on 2026-10-06, when the list grew to 20 repositories), which makes an old key look new. The rotation script therefore records two organization variables, visible to `github_actions` only:

- `TF_AUTOMATION_APP_KEY_FINGERPRINT`: the `SHA256:` fingerprint of the stored key, in the format the App's settings page shows;
- `TF_AUTOMATION_APP_KEY_ROTATED_AT`: when the script stored it, as an ISO 8601 UTC timestamp (a plain `YYYY-MM-DD` date is accepted too).

The health check reads both through the workflow's `vars` context, so it needs no extra App permission, and it fingerprints the key in `TF_AUTOMATION_APP_PRIVATE_KEY` itself. Only when the two fingerprints match does the key's age count from the record. Otherwise it falls back to the secret's `updated_at` and says so in the report: as a note when there is no record yet, and as a warning when the record describes another key (the secret was replaced without the script) or is incomplete or malformed.

The key in use when this was introduced predated the script's record, so its record was created by hand on 2026-10-07, dating the key to 2026-10-06. If a key ever lacks a record again, create one with the fingerprint the health report shows under "App private key" and the date the key was generated:

```bash
gh variable set TF_AUTOMATION_APP_KEY_FINGERPRINT --org ThreatFlux --visibility selected --repos github_actions --body 'SHA256:<fingerprint from the health report>'
gh variable set TF_AUTOMATION_APP_KEY_ROTATED_AT --org ThreatFlux --visibility selected --repos github_actions --body '2026-10-06'
```

From then on every rotation updates both. If the script cannot write them, it prints the same two commands with the new values.

## Health check

[`automation-app-health.yml`](../.github/workflows/automation-app-health.yml) runs every Monday at 07:17 UTC and on demand (`gh workflow run automation-app-health.yml -R ThreatFlux/github_actions`). It mints installation tokens with the stored key, each limited to the permission its check needs:

| Check | Token permission | Failure means |
| --- | --- | --- |
| Mint a token | `metadata: read` | **broken**: the key or App ID is wrong, or the secret is not shared with `github_actions`; the run fails |
| List the installation's repositories and compare them with `.github/automation-app-repos.txt` | `metadata: read` | **broken**: a repository was added to or removed from the installation without the list (or the other way round); the run fails |
| Compare the `TF_AUTOMATION_APP_PRIVATE_KEY` secret's selected repositories with the list | `organization_secrets: read` | **broken**: a listed repository cannot read the key, an unlisted one can, or the secret is not a `selected` organization secret; the run fails |
| Compare the `TF_AUTOMATION_APP_ID` variable's selected repositories with the list | `organization_actions_variables: read` | **broken** on any difference or when it is not an organization variable, as for the secret. **attention** while the App lacks the permission (see below) |
| List the organization's secrets (names, dates, and visibility only) | `organization_secrets: read` | **attention**: the App private key is older than 90 days (from its [rotation record](#key-age-tracking) when one matches), another credential is older than 180 days (name-only secrets such as `DOCKERHUB_USERNAME` are exempt), a secret being retired (`GIT_TOKEN`, `CODECOV_TOKEN`) still exists, or one of the [deleted secrets](#deleted-secrets) exists again. The report's "Deleted secrets" row shows how many of them are still deleted |
| Fingerprint the stored key and compare it with the rotation record | none (reads the secret it already holds) | **attention** when the record names a different key or is malformed; a missing record is only a note |

Together the three comparisons assert that the installation, the key secret, and the App ID variable reach the same repositories, the ones in the list.

The secret audit needs the App's organization permission **Secrets: Read-only**, and the variable comparison needs **Variables: Read-only**. An organization owner grants each in the App's settings (Permissions and events, Organization permissions) and then approves the updated permissions on the installation. Until then the run reports the skipped check as a finding instead of failing silently. `actions/create-github-app-token` v3.2.0 does not list the `permission-organization-actions-variables` input yet but forwards it to the token request, so its "Unexpected input" warning in the run log is expected.

The workflow has no `pull_request` trigger, because a pull request would run its own copy of a job that holds the App key. Its evaluation logic is unit tested in CI instead.

Findings go to a single issue labelled `secret-rotation` in this repository, created with the workflow's `GITHUB_TOKEN` (`issues: write`). Each run rewrites the issue body with the latest report, reopens the issue if it was closed and something is wrong again, and closes it once a run finds nothing but notes.
