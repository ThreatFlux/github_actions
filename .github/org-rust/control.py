"""Plan public allowlisted Rust updates without executing repository code."""

import base64
import datetime
import hashlib
import json
import re

from adapters import patch_file, validate_file, version_tuple
from api import PolicyError, object_id, sha
from targets import VARIANTS, validate_target

CENTRAL = "ThreatFlux/github_actions"
POLICY_PATH = ".github/org-rust/policy.json"
NAMESPACE = "codex/org-rust-"


def fingerprint(value):
    return hashlib.sha256(
        json.dumps(value, sort_keys=True, separators=(",", ":")).encode()
    ).hexdigest()


def validate_policy(policy):
    if policy.get("schema_version") != 1 or policy.get("organization") != "ThreatFlux":
        raise PolicyError("Only the reviewed ThreatFlux policy is supported")
    if policy["app"] != {"id": 5205173, "slug": "threatflux-automation"}:
        raise PolicyError("Unexpected automation App identity")
    if type(policy.get("writes_enabled")) is not bool:
        raise PolicyError("Write activation must be a reviewed Boolean policy field")
    version_tuple(policy["target"]["stable"]["version"])
    if set(policy["target"]["images"]) != VARIANTS:
        raise PolicyError(
            "Compiler policy must include every supported official image variant"
        )
    names, identities = set(), set()
    for repository in policy["repositories"]:
        validate_repository(repository, names, identities)


def validate_repository(repository, names, identities):
    name = repository["repository"]
    if not re.fullmatch(r"ThreatFlux/[A-Za-z0-9_.-]+", name):
        raise PolicyError("Invalid public allowlist repository name")
    if (
        name in names
        or repository["id"] in identities
        or type(repository["id"]) is not int
    ):
        raise PolicyError("Duplicate or invalid allowlist repository identity")
    if repository["default_branch"] != "main" or not repository["files"]:
        raise PolicyError(
            "A maintained adapter and reviewed default branch are required"
        )
    names.add(name)
    identities.add(repository["id"])
    for file in repository["files"]:
        validate_adapter_file(file)


def validate_adapter_file(file):
    validate_file(file["path"])
    if file["mode"] not in {"100644", "100755"} or not file["adapters"]:
        raise PolicyError("Invalid maintained regular-file adapter")
    for adapter in file["adapters"]:
        if type(adapter["count"]) is not int or adapter["count"] < 1:
            raise PolicyError("Invalid maintained selector match count")


def select_repositories(policy, organization, subset):
    validate_policy(policy)
    if organization != policy["organization"]:
        raise PolicyError("Organization is outside the reviewed policy")
    wanted = {name.strip() for name in subset.split(",") if name.strip()}
    candidates = policy["repositories"]
    available = {item["repository"].split("/", 1)[1] for item in candidates}
    if not wanted <= available:
        raise PolicyError(
            "Requested subset is outside the public maintained-Rust allowlist"
        )
    return [
        item
        for item in candidates
        if not wanted or item["repository"].split("/")[1] in wanted
    ]


def repository_source(github, item):
    name = item["repository"]
    metadata = github.api("/repos/" + name)
    expected = (item["id"], name, item["default_branch"])
    actual = (metadata["id"], metadata["full_name"], metadata["default_branch"])
    if (
        actual != expected
        or metadata["private"]
        or metadata["fork"]
        or metadata["archived"]
    ):
        raise PolicyError(
            "Public repository identity, ownership or default branch changed"
        )
    head = github.api(f"/repos/{name}/git/ref/heads/{item['default_branch']}")
    if (
        head["ref"] != "refs/heads/" + item["default_branch"]
        or head["object"]["type"] != "commit"
    ):
        raise PolicyError("Invalid current default-branch reference")
    return metadata, sha(head["object"]["sha"])


def unchanged_head(github, item, expected):
    _, current = repository_source(github, item)
    if current != expected:
        raise PolicyError("Current default branch moved; recompute the read-only plan")


def planned_file(github, repository, file, tree, policy):
    path = file["path"]
    entry = tree.get(path)
    if not entry or entry["mode"] != file["mode"]:
        raise PolicyError(
            "Maintained input is missing or its regular-file mode changed"
        )
    for index in range(1, len(path.split("/"))):
        parent = tree.get("/".join(path.split("/")[:index]))
        if not parent or parent["type"] != "tree" or parent["mode"] != "040000":
            raise PolicyError("Maintained input has a non-directory parent")
    original = github.blob(repository, entry)
    try:
        proposed = patch_file(
            original.decode("utf-8"), file["adapters"], policy
        ).encode("utf-8")
    except PolicyError as error:
        raise PolicyError(f"{repository}:{path}: {error}") from error
    if proposed == original:
        return None
    return {
        "path": path,
        "mode": entry["mode"],
        "base_blob_sha": entry["sha"],
        "proposed_blob_sha": object_id(proposed),
        "before_sha256": hashlib.sha256(original).hexdigest(),
        "after_sha256": hashlib.sha256(proposed).hexdigest(),
        "content": base64.b64encode(proposed).decode("ascii"),
    }


def repository_plan(github, item, policy):
    metadata, source = repository_source(github, item)
    tree_sha, tree = github.tree(item["repository"], source)
    changes = [
        planned_file(github, item["repository"], file, tree, policy)
        for file in item["files"]
    ]
    changes = [change for change in changes if change]
    unchanged_head(github, item, source)
    return {
        "repository": item["repository"],
        "repository_id": metadata["id"],
        "default_branch": item["default_branch"],
        "source_sha": source,
        "tree_sha": tree_sha,
        "status": "change" if changes else "no-op",
        "changes": changes,
        "needs_workflows_write": any(
            change["path"].startswith(".github/workflows/") for change in changes
        ),
    }


def plan(github, policy, organization, subset, official):
    selected = select_repositories(policy, organization, subset)
    validate_target(policy, official)
    rows = [repository_plan(github, item, policy) for item in selected]
    return {
        "schema_version": 1,
        "policy_sha256": fingerprint(policy),
        "target": official,
        "checked_at_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "organization": organization,
        "complete": True,
        "writes_enabled": policy["writes_enabled"] is True,
        "changed_repositories": sum(row["status"] == "change" for row in rows),
        "repositories": rows,
    }


def central_main(github, expected):
    current = github.api(f"/repos/{CENTRAL}/git/ref/heads/main")["object"]["sha"]
    if sha(current) != sha(expected):
        raise PolicyError(
            "Central main moved; only its reviewed current policy may write"
        )


def app_access(github, policy, row, slug):
    if slug != policy["app"]["slug"]:
        raise PolicyError("Minted App token has an unexpected provider identity")
    app = github.api("/apps/" + slug)
    if (
        app["id"] != policy["app"]["id"]
        or app["owner"]["login"] != policy["organization"]
    ):
        raise PolicyError("Automation App identity changed")
    if row["needs_workflows_write"] and app["permissions"].get("workflows") != "write":
        raise PolicyError(
            "Changed workflow selectors require an explicit maintainer grant of App Workflows: write"
        )
    installed = github.pages("/installation/repositories", "repositories")
    if {repo["id"] for repo in installed} != {row["repository_id"]}:
        raise PolicyError(
            "App token must be scoped to exactly this public target repository"
        )
    return {
        "id": app["id"],
        "slug": slug,
        "permissions": app["permissions"],
        "scoped_repository_id": row["repository_id"],
    }


def capability_profile(github, policy, report):
    app = github.api("/apps/" + policy["app"]["slug"])
    if (
        app["id"] != policy["app"]["id"]
        or app["owner"]["login"] != policy["organization"]
    ):
        raise PolicyError("Automation App identity changed")
    report["app_profile"] = {
        "id": app["id"],
        "slug": app["slug"],
        "permissions": app["permissions"],
    }
    capable = app["permissions"].get("workflows") == "write"
    blocked = [
        row["repository"]
        for row in report["repositories"]
        if row["needs_workflows_write"] and not capable
    ]
    report["workflow_write_profile_configured"] = capable
    report["blocked_workflow_permission_repositories"] = blocked
    report["complete"] = report["complete"] and not blocked
    return report


def branch_name(policy, policy_only=False):
    return (
        NAMESPACE
        + ("policy-" if policy_only else "")
        + policy["target"]["stable"]["version"]
        + "-"
        + fingerprint(policy)[:12]
    )


def candidate_proposal(github, policy, row, branch):
    repository = row["repository"]
    pulls = github.pages(
        f"/repos/{repository}/pulls?state=open&base={row['default_branch']}"
    )
    ours = [
        pull
        for pull in pulls
        if (pull["head"].get("repo") or {}).get("id") == row["repository_id"]
        and pull["head"]["ref"].startswith(NAMESPACE)
    ]
    if not ours:
        return None
    if len(ours) != 1 or ours[0]["head"]["ref"] != branch:
        raise PolicyError(
            "Another organization-Rust proposal needs maintainer review first"
        )
    pull = ours[0]
    if (
        pull["user"]["login"] != policy["app"]["slug"] + "[bot]"
        or pull["head"]["repo"]["id"] != row["repository_id"]
    ):
        raise PolicyError(
            "Existing toolchain branch is not owned by the configured App"
        )
    return pull


def existing_proposal(github, policy, row, branch):
    pull = candidate_proposal(github, policy, row, branch)
    if pull is None:
        return None
    repository = row["repository"]
    ref = github.api(f"/repos/{repository}/git/ref/heads/{branch}")
    if ref["object"]["sha"] != pull["head"]["sha"]:
        raise PolicyError("Existing proposal head moved concurrently")
    compare = github.api(
        f"/repos/{repository}/compare/{row['source_sha']}...{pull['head']['sha']}"
    )
    expected = {
        change["path"]: change["proposed_blob_sha"] for change in row["changes"]
    }
    actual = {file["filename"]: file["sha"] for file in compare.get("files", [])}
    if compare["status"] != "ahead" or actual != expected:
        raise PolicyError(
            "Existing proposal differs or its base advanced; preserve it for maintainer review"
        )
    commit = github.api(f"/repos/{repository}/commits/{pull['head']['sha']}")
    if (
        not commit["commit"]["verification"]["verified"]
        or commit["author"]["login"] != policy["app"]["slug"] + "[bot]"
    ):
        raise PolicyError("Existing proposal lacks a verified attributable App commit")
    verify_proposal_tree(github, row, pull["head"]["sha"], commit)
    return {
        "status": "existing-proposal",
        "pull_request": pull["html_url"],
        "head_sha": pull["head"]["sha"],
    }


def verify_proposal_tree(github, row, commit_sha, commit):
    if [parent["sha"] for parent in commit["parents"]] != [row["source_sha"]]:
        raise PolicyError(
            "Created proposal does not have the exact expected source parent"
        )
    _, original = github.tree(row["repository"], row["source_sha"])
    _, proposed = github.tree(row["repository"], commit_sha)
    expected = {
        path: (entry["mode"], entry["type"], entry["sha"])
        for path, entry in original.items()
        if entry["type"] != "tree"
    }
    for change in row["changes"]:
        if expected.get(change["path"]) != (
            change["mode"],
            "blob",
            change["base_blob_sha"],
        ):
            raise PolicyError("Original proposal input mode or identity differs")
        expected[change["path"]] = (change["mode"], "blob", change["proposed_blob_sha"])
    actual = {
        path: (entry["mode"], entry["type"], entry["sha"])
        for path, entry in proposed.items()
        if entry["type"] != "tree"
    }
    if actual != expected:
        raise PolicyError(
            "Created signed commit changes unexpected paths, file modes or bytes"
        )


def signed_commit(github, row, branch, policy, policy_commit, central_github=None):
    additions = [
        {"path": change["path"], "contents": change["content"]}
        for change in row["changes"]
    ]
    mutation = """mutation($input:CreateCommitOnBranchInput!){createCommitOnBranch(input:$input){commit{oid}}}"""
    version = policy["target"]["stable"]["version"]
    central_main(central_github or github, policy_commit)
    result = github.graphql(
        mutation,
        {
            "input": {
                "branch": {
                    "repositoryNameWithOwner": row["repository"],
                    "refName": branch,
                },
                "expectedHeadOid": row["source_sha"],
                "message": {
                    "headline": f"chore(ci): {'propose' if row.get('policy_only') else 'adopt'} organization Rust {version}"
                    + (" policy" if row.get("policy_only") else ""),
                    "body": f"Reviewed central control plane: {CENTRAL}@{policy_commit}\nPolicy-SHA256: {fingerprint(policy)}\nBase-SHA: {row['source_sha']}",
                },
                "fileChanges": {"additions": additions},
            }
        },
    )
    commit_sha = sha(result["createCommitOnBranch"]["commit"]["oid"])
    commit = github.api(f"/repos/{row['repository']}/commits/{commit_sha}")
    if (
        not commit["commit"]["verification"]["verified"]
        or commit["author"]["login"] != policy["app"]["slug"] + "[bot]"
    ):
        raise PolicyError("GitHub did not produce a verified attributable App commit")
    verify_proposal_tree(github, row, commit_sha, commit)
    return commit_sha


def pull_payload(row, branch, policy, policy_commit):
    version = policy["target"]["stable"]["version"]
    body = f"Apply the reviewed [organization Rust policy](https://github.com/{CENTRAL}/blob/{policy_commit}/{POLICY_PATH}) to exact base `{row['source_sha']}`. Only maintained stable compiler selectors and their coupled verified official image pins change. Cargo MSRV, beta/nightly, legacy/fixture lanes and package versions are preserved."
    if row.get("policy_only"):
        body = f"Propose official stable Rust {version} and all matching verified two-architecture official image digests in the central policy. The trusted discovery code is at `{CENTRAL}@{policy_commit}`. No target repository is changed by this proposal; rollout waits for normal review and policy merge."
    return {
        "title": f"chore(ci): {'propose' if row.get('policy_only') else 'adopt'} organization Rust {version}"
        + (" policy" if row.get("policy_only") else ""),
        "head": branch,
        "base": row["default_branch"],
        "maintainer_can_modify": False,
        "body": body
        + f"\n\nPolicy SHA256: `{fingerprint(policy)}`. Existing full hosted CI and normal review/merge/publication rules remain required; this updater does not approve or merge PRs.",
    }


def write_proposal(github, policy, row, policy_commit, central_github=None):
    central = central_github or github
    central_main(central, policy_commit)
    repository = row["repository"]
    item = next(
        item for item in policy["repositories"] if item["repository"] == repository
    )
    branch = branch_name(policy, row.get("policy_only", False))
    existing = existing_proposal(github, policy, row, branch)
    if existing:
        unchanged_head(github, item, row["source_sha"])
        central_main(central, policy_commit)
        return existing
    if github.api(f"/repos/{repository}/git/ref/heads/{branch}", missing=True):
        raise PolicyError(
            "An existing branch has no verified owned proposal; preserve it"
        )
    unchanged_head(github, item, row["source_sha"])
    central_main(central, policy_commit)
    github.api(
        f"/repos/{repository}/git/refs",
        {"ref": "refs/heads/" + branch, "sha": row["source_sha"]},
    )
    commit_sha = signed_commit(github, row, branch, policy, policy_commit, central)
    unchanged_head(github, item, row["source_sha"])
    head = github.api(f"/repos/{repository}/git/ref/heads/{branch}")["object"]["sha"]
    if head != commit_sha:
        raise PolicyError(
            "Toolchain branch moved before PR creation; preserve all work"
        )
    central_main(central, policy_commit)
    pull = github.api(
        f"/repos/{repository}/pulls", pull_payload(row, branch, policy, policy_commit)
    )
    return {
        "status": "proposed",
        "pull_request": pull["html_url"],
        "head_sha": commit_sha,
    }
