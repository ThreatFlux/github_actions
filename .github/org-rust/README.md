# Organization Rust policy

This control plane proposes Rust compiler updates through ordinary pull requests.
The reviewed [policy](policy.json) contains the stable compiler's official channel
checksum/release metadata, four official Rust image variants with both Linux
architectures, and exact maintained-field adapters for 21 public Rust repositories.
Every repository outside this public allowlist is denied by default. Repository
names, IDs and metadata discovered outside that allowlist are never published.

The initial target is Rust 1.100.0. Writes start disabled. The Rust CLI, Docker
action, release engine and consumer publisher pins are unchanged by this control
plane. Other tool families require separate reviewed adapters.

## Discovery and rollout

[Policy Discovery](../workflows/org-rust-policy.yml) reads the official stable
channel and its SHA256 sidecar, confirms the stable GitHub release, and resolves
each official image index, both architecture manifests and their configuration
digests. Missing images, different compiler metadata, wrong architectures and
digest/size mismatches stop the proposal. A same-compiler official image rebuild
may propose new image digests when the compiler metadata stays exactly unchanged.
A reviewed policy PR must merge before target repository updates are possible.
After activation, scheduled discovery may open that central policy PR. Its merge
starts a read-only rollout plan; operators explicitly dispatch the pilot subset
and, after reviewing its hosted results, the remaining targets.

[Rust Rollout](../workflows/org-rust-rollout.yml) emits a read-only plan containing
actual repository IDs, current default-branch commits/trees, selected old/new blob
identities and precise proposed bytes. It accepts `organization` (currently only
`ThreatFlux`), an optional comma-separated subset of public allowlist repository
names, and `dry_run` (default true). The plan job uses only the read-only
`GITHUB_TOKEN`; it never mints the App key or checks out or executes target code.
The source plan's `complete` field describes verified repository inputs and
proposed bytes. Its separate `installed_capability.status` stays `UNVERIFIED`:
read-only planning does not query private App metadata or claim permission grants.

Each declared adapter binds one value span to its exact literal context and
workflow field/job scope. Only those spans change. Compiler and image references
are updated together; a FROM tag and its index digest form one image span.
Accepted previous states let the same adapters process 1.100 to 1.101 to 1.102
and recognize repeated runs. Cargo `rust-version`, MSRV, beta/nightly, vendor,
legacy, fuzz and historical fixture inputs are preserved. A missing/ambiguous
context, changed file mode or unrecognized newer selector stops the rollout
instead of guessing. Layout changes require a reviewed adapter change.

## Credentials and activation

Credentialed execution runs only from reviewed, exact-current `main`. The existing
`TF_AUTOMATION_APP_ID` variable and `TF_AUTOMATION_APP_PRIVATE_KEY` secret mint
tokens through the pinned `actions/create-github-app-token` action. Each rollout
token is restricted to one selected public repository and explicitly requests
Contents write, Pull requests write and Metadata read. Workflows write is
requested only when the actual proposed paths include `.github/workflows/`;
providing the explicit permission map prevents inherited broad App permissions.
Before minting, the trusted job binds the resolved App variable to either the
reviewed numeric App ID or its exact nonsecret client-ID mapping in the policy.
After a successful mint, the writer verifies the pinned JWT-authenticated
action's App slug and positive installation ID and reads the token's exact
one-repository installation scope. It does not call the private App-profile
endpoint with a read-only or installation token. The completed pinned action's
explicit permission request, together with provider/scope readback, is the
installed-capability evidence; a profile alone cannot grant that evidence.
Policy-discovery PRs request no workflow permission and are scoped only here.

The owner-verified App profile at initial deployment did not grant Workflows write.
Actual compiler updates
that modify workflow selectors are therefore blocked until a maintainer grants
that repository permission and approves it for the installation. This program
never changes App permissions, organization settings or token scopes itself.
The profile alone does not prove installation approval. After an approved grant,
dispatch Rust Rollout on `main` with a representative public subset and
`probe_workflows_permission: true`. The separate probe must successfully mint a
token explicitly requesting Workflows write and complete the provider/scope/source
readback. It creates no commits, refs, tags or PRs. A failed mint remains a blocker.

Activation follows these steps:

1. Merge this control plane after its exact-head hosted checks and source review.
2. Run the main read-only plan. Initial current Rust 1.100.0 targets should be
   genuine no-ops once their independently reviewed upgrades are accepted.
3. Review that source-bound plan, then enable `writes_enabled` in a normal policy
   PR. Rust Rollout's policy pushes and scheduled runs remain read-only. Only an
   explicit main dispatch with `dry_run: false` invokes the scoped write path.
4. Prove repeated current-target write-mode no-ops with zero mutations. Do not
   create empty/canary PRs or bump a compiler merely to demonstrate the writer.
5. Verify the installed Workflows capability as above before claiming future
   changed-compiler fanout is fully operational. Start changed rollouts with a
   representative library and SDK by supplying `repositories`, review their
   diffs/full hosted results, then explicitly dispatch the remaining subset or
   the full allowlist. Merging a changed target policy does not fan out PRs
   before this pilot review.

## Branch ownership, signing and recovery

Target branches use `codex/org-rust-<version>-<policy-hash>`. Central policy
proposals use `codex/org-rust-policy-<version>-<policy-hash>`. They never share the
package-release or explicit manual-version namespaces. The writer creates
[GitHub-signed commits](https://docs.github.com/en/graphql/reference/commits#createcommitonbranch)
with `expectedHeadOid`, then verifies the App author, signature, exact source
parent and entire resulting file tree (paths, modes and blob IDs) before opening
a PR. It rechecks default and proposal heads and never force-updates main,
self-approves, merges, bypasses protections or changes publication conventions.

An identical existing App-owned, verified, exact-base proposal is reused without
mutation. A human-owned branch, changed proposal, advanced base, concurrent head
movement or older pending toolchain proposal stops for maintainer review. If a
network failure leaves a bot branch without a PR, the writer preserves it and
reports the collision. A maintainer must inspect its commit/parent/tree and
recover it through normal policy; the updater does not force-reset or delete it.

Both target PR checks and post-merge release/no-release behavior stay owned by
each repository. A generated PR is a proposal, not evidence of successful CI or
publication. The configured workflows retain actual source-bound JSON reports;
the control-plane regression suite runs on GitHub, not as a local publishing gate.

Primary boundaries: [GitHub App permissions](https://docs.github.com/en/rest/authentication/permissions-required-for-github-apps),
[official Docker CDN allowlist](https://docs.docker.com/desktop/enterprise/allow-list/).
GitHub/channel redirects are rejected. Official public Docker config-blob
redirects are restricted to the Docker CDN, strip authorization/cookies, and
still require the expected immutable SHA256 and byte size.
