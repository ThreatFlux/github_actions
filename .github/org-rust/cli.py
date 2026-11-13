"""Trusted central entry point; plans never check out or execute target code."""

import argparse
import base64
import datetime
import json
import os
from pathlib import Path

from api import GitHub, PolicyError, object_id, sha
from control import (
    CENTRAL,
    POLICY_PATH,
    app_access,
    app_identifier,
    capability_requirements,
    central_main,
    fingerprint,
    plan,
    repository_plan,
    repository_source,
    select_repositories,
    validate_policy,
    write_proposal,
)
from targets import official_target


def load(path):
    return json.loads(Path(path).read_text(encoding="utf-8"))


def emit(path, result):
    Path(path).write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    summary = {key: value for key, value in result.items() if key != "repositories"}
    summary.pop("target", None)
    summary.pop("proposed_policy", None)
    print(json.dumps(summary), flush=True)


def outputs(values):
    output = os.environ.get("GITHUB_OUTPUT")
    if output:
        with open(output, "a", encoding="utf-8") as handle:
            for key, value in values.items():
                handle.write(f"{key}={json.dumps(value, separators=(',', ':'))}\n")


def stamp(result, args):
    result.update(
        policy_commit=sha(args.expected_policy_commit),
        run_id=os.environ.get("GITHUB_RUN_ID", "local-read-only"),
    )
    return result


def read_plan(args, policy, github):
    result = stamp(
        plan(
            github,
            policy,
            args.organization,
            args.repositories,
            official_target(github),
        ),
        args,
    )
    capability_requirements(policy, result)
    emit(args.output, result)
    matrix = [
        {
            "repository": row["repository"].split("/", 1)[1],
            "needs_workflows_write": row["needs_workflows_write"],
        }
        for row in result["repositories"]
    ]
    outputs(
        {
            "matrix": {"include": matrix},
            "enabled": result["writes_enabled"],
            "changed": result["changed_repositories"] > 0,
        }
    )
    if not result["complete"]:
        raise PolicyError("The read-only repository source plan is incomplete")


def minted_identity():
    return {
        "identifier": os.environ["GITHUB_APP_IDENTIFIER"],
        "slug": os.environ["GITHUB_APP_SLUG"],
        "installation_id": os.environ["GITHUB_APP_INSTALLATION_ID"],
        "workflows": os.environ["GITHUB_APP_WORKFLOWS_PERMISSION"],
    }


def trusted_plan(args, policy):
    prior = load(args.plan)
    if not policy["writes_enabled"] or prior.get("complete") is not True:
        raise PolicyError(
            "Writes require reviewed activation after a successful main read-only plan"
        )
    expected = (
        fingerprint(policy),
        args.expected_policy_commit,
        os.environ.get("GITHUB_RUN_ID"),
    )
    actual = (
        prior.get("policy_sha256"),
        prior.get("policy_commit"),
        prior.get("run_id"),
    )
    if actual != expected or prior.get("target") != policy["target"]:
        raise PolicyError(
            "Write input is not this trusted run's exact reviewed policy plan"
        )
    checked = datetime.datetime.fromisoformat(prior["checked_at_utc"])
    age = datetime.datetime.now(datetime.timezone.utc) - checked
    if not datetime.timedelta(0) <= age <= datetime.timedelta(hours=1):
        raise PolicyError("Read-only plan expired; recompute before writing")
    return prior


def apply(args, policy, github):
    prior = trusted_plan(args, policy)
    selected = select_repositories(policy, args.organization, args.repositories)
    if len(selected) != 1:
        raise PolicyError(
            "Each write job must mint a token scoped to exactly one public target"
        )
    item = selected[0]
    row = repository_plan(github, item, policy)
    planned = [
        row for row in prior["repositories"] if row["repository"] == item["repository"]
    ]
    if len(planned) != 1 or fingerprint(row) != fingerprint(planned[0]):
        raise PolicyError("Target moved or proposal changed after the read-only plan")
    central = GitHub(os.environ["CENTRAL_GH_TOKEN"])
    central_main(central, args.expected_policy_commit)
    provider = app_access(github, policy, row, minted_identity())
    result = {
        "repository": row["repository"],
        "source_sha": row["source_sha"],
        "policy_commit": args.expected_policy_commit,
        "app_access": provider,
    }
    result.update(
        write_proposal(github, policy, row, args.expected_policy_commit, central)
        if row["changes"]
        else {"status": "no-op", "mutations": 0}
    )
    emit(args.output, result)


def refreshed_policy(policy, target):
    proposed = json.loads(json.dumps(policy))
    if target != policy["target"]:
        from adapters import version_tuple

        new_version = version_tuple(target["stable"]["version"])
        old_version = version_tuple(policy["target"]["stable"]["version"])
        if new_version < old_version or (
            new_version == old_version
            and target["stable"] != policy["target"]["stable"]
        ):
            raise PolicyError(
                "Official compiler metadata changed unexpectedly or would downgrade Rust"
            )
        prior = [*policy["accepted_states"], policy["target"]]
        proposed["accepted_states"] = list(
            {fingerprint(state): state for state in prior}.values()
        )
        proposed["target"] = target
    return proposed


def refresh(args, policy, github):
    validate_policy(policy)
    if args.organization != policy["organization"]:
        raise PolicyError("Organization is outside the reviewed policy")
    proposed = refreshed_policy(policy, official_target(github))
    result = stamp(
        {
            "complete": True,
            "changed": proposed != policy,
            "writes_enabled": policy["writes_enabled"],
            "policy_sha256": fingerprint(policy),
            "checked_at_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
            "target": policy["target"],
            "proposed_policy": proposed,
        },
        args,
    )
    emit(args.output, result)
    outputs({"changed": result["changed"], "enabled": policy["writes_enabled"]})


def validated_refresh(github, policy, prior):
    # Rebuild public policy bytes from trusted source plus fresh official data.
    # The artifact is a comparison input, never the source of a commit's bytes.
    proposed = refreshed_policy(policy, official_target(github))
    validate_policy(proposed)
    if prior["proposed_policy"] != proposed:
        raise PolicyError(
            "Proposed policy includes unreviewed edits or stale official images"
        )
    return proposed


def central_policy_row(github, policy, proposed):
    item = next(
        item for item in policy["repositories"] if item["repository"] == CENTRAL
    )
    _, source = repository_source(github, item)
    _, tree = github.tree(CENTRAL, source)
    old = github.blob(CENTRAL, tree[POLICY_PATH])
    if fingerprint(json.loads(old)) != fingerprint(policy):
        raise PolicyError("Current central policy differs from reviewed input")
    content = (json.dumps(proposed, indent=2) + "\n").encode()
    return {
        "repository": CENTRAL,
        "repository_id": item["id"],
        "default_branch": item["default_branch"],
        "source_sha": source,
        "needs_workflows_write": False,
        "policy_only": True,
        "changes": [
            {
                "path": POLICY_PATH,
                "mode": tree[POLICY_PATH]["mode"],
                "base_blob_sha": tree[POLICY_PATH]["sha"],
                "proposed_blob_sha": object_id(content),
                "content": base64.b64encode(content).decode("ascii"),
            }
        ],
    }


def apply_refresh(args, policy, github):
    prior = trusted_plan(args, policy)
    if prior.get("changed") is not True:
        emit(args.output, {"status": "no-op", "mutations": 0})
        return
    proposed = validated_refresh(github, policy, prior)
    row = central_policy_row(github, policy, proposed)
    provider = app_access(github, policy, row, minted_identity())
    result = write_proposal(github, proposed, row, args.expected_policy_commit)
    emit(
        args.output,
        {
            **result,
            "app_access": provider,
            "scope": "central-policy proposal only; fanout waits for normal review/merge",
        },
    )


def probe(args, policy, github):
    selected = select_repositories(policy, args.organization, args.repositories)
    if len(selected) != 1:
        raise PolicyError(
            "The capability probe requires one public allowlisted repository"
        )
    metadata, source = repository_source(github, selected[0])
    row = {"repository_id": metadata["id"], "needs_workflows_write": True}
    provider = app_access(github, policy, row, minted_identity())
    emit(
        args.output,
        {
            "status": "scoped-workflows-token-readback",
            "repository": metadata["full_name"],
            "source_sha": source,
            "app_access": provider,
            "requested_permissions": {
                "workflows": "write",
                "contents": "read",
                "metadata": "read",
            },
            "mutations": 0,
        },
    )


def arguments():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "command",
        choices=["plan", "apply", "refresh", "apply-refresh", "preflight", "probe"],
    )
    parser.add_argument("--policy", default=POLICY_PATH)
    parser.add_argument("--organization", default="ThreatFlux")
    parser.add_argument(
        "--repositories", default="", help="Comma-separated public allowlist names only"
    )
    parser.add_argument("--expected-policy-commit", required=True)
    parser.add_argument("--plan")
    parser.add_argument("--output", default="org-rust-report.json")
    return parser.parse_args()


def main():
    args = arguments()
    policy = load(args.policy)
    validate_policy(policy)
    github = GitHub(os.environ.get("GH_TOKEN"))
    commands = {
        "plan": read_plan,
        "apply": apply,
        "refresh": refresh,
        "apply-refresh": apply_refresh,
        "probe": probe,
    }
    if args.command == "preflight":
        central_main(github, args.expected_policy_commit)
        app_identifier(policy, os.environ.get("GITHUB_APP_IDENTIFIER"))
        emit(
            args.output,
            {"status": "exact-current-main", "source_sha": args.expected_policy_commit},
        )
    else:
        commands[args.command](args, policy, github)


if __name__ == "__main__":
    try:
        main()
    except (PolicyError, KeyError, ValueError, TypeError, OSError) as error:
        raise SystemExit("Organization Rust policy stopped: " + str(error)) from error
