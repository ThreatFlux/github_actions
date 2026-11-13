#!/usr/bin/env python3
"""Admit only explicit upstream, source-bound maintainer candidate images."""

import json
import os
import re

# B404: Only fixed gh/git argv lists are executed; no shell evaluates inputs.
import subprocess  # nosec B404
from pathlib import Path

UPSTREAM = "ThreatFlux/github_actions"
SHA = re.compile(r"[0-9a-f]{40}")
WRITERS = {"admin", "maintain", "write"}


def candidate_source(context):
    """Reject ambiguous flags, release requests and non-upstream dispatches."""
    enabled = context.get("CANDIDATE_RUNTIME", "false")
    source = context.get("CANDIDATE_SOURCE_SHA", "")
    if enabled not in {"true", "false"}:
        raise ValueError("candidate mode must be an explicit boolean")
    if enabled == "false":
        if source:
            raise ValueError("candidate source requires explicit candidate mode")
        return None
    validate_dispatch(context)
    if not SHA.fullmatch(source) or context.get("GITHUB_SHA") != source:
        raise ValueError("candidate source must equal the full dispatch SHA")
    reviewed_ref(context)
    return source


def validate_dispatch(context):
    if context.get("RELEASE_PREP") != "false":
        raise ValueError("a candidate cannot prepare or hand off a release")
    if context.get("GITHUB_EVENT_NAME") != "workflow_dispatch":
        raise ValueError("candidate images require an explicit manual dispatch")
    if context.get("GITHUB_REPOSITORY") != UPSTREAM:
        raise ValueError("candidate runtime publication is upstream-only")
    if context.get("REPOSITORY_IS_FORK") != "false":
        raise ValueError("candidate runtime publication cannot run from a fork")


def reviewed_ref(context):
    ref = context.get("GITHUB_REF", "")
    if not ref.startswith("refs/heads/codex/") or ref == "refs/heads/codex/":
        raise ValueError("candidate dispatch must target a reviewed codex branch")
    if context.get("GITHUB_REF_NAME") != ref.removeprefix("refs/heads/"):
        raise ValueError("dispatch ref and ref name disagree")


def trusted_actors(context):
    """Check both the original dispatcher and a possible rerun initiator."""
    actors = {
        context.get("GITHUB_ACTOR", ""),
        context.get("GITHUB_TRIGGERING_ACTOR", ""),
    }
    if any(not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9-]*", actor) for actor in actors):
        raise ValueError("candidate dispatch actor identity is missing or invalid")
    return sorted(actors)


def validate_evidence(source, checkout, remote_ref, permissions):
    """Do not publish a different checkout or a branch that advanced."""
    if checkout != source or remote_ref.get("object", {}).get("sha") != source:
        raise ValueError("candidate checkout and current remote ref must match source")
    if not permissions or any(p.get("permission") not in WRITERS for p in permissions):
        raise ValueError(
            "every dispatch actor must currently have repository write permission"
        )


def api(endpoint):
    # B603: Fixed gh/api command; upstream is constant and actor/ref are validated.
    payload = subprocess.check_output(  # nosec B603
        ["gh", "api", endpoint], text=True, shell=False
    )
    return json.loads(payload)


def admit(context):
    source = candidate_source(context)
    if source is None:
        return {"enabled": "false"}
    actors = trusted_actors(context)
    permissions = [
        api(f"repos/{UPSTREAM}/collaborators/{actor}/permission") for actor in actors
    ]
    remote = api(f"repos/{UPSTREAM}/git/ref/heads/{context['GITHUB_REF_NAME']}")
    # B603: Entire Git argv is constant; no user-supplied Git options or shell.
    checkout = subprocess.check_output(  # nosec B603
        ["git", "rev-parse", "--verify", "HEAD"], text=True, shell=False
    ).strip()
    validate_evidence(source, checkout, remote, permissions)
    return {
        "enabled": "true",
        "source": source,
        "image": f"ghcr.io/{UPSTREAM.lower()}:candidate-{source}",
    }


def main():
    outputs = admit(os.environ)
    with Path(os.environ["GITHUB_OUTPUT"]).open("a", encoding="utf-8") as output:
        for key, value in outputs.items():
            output.write(f"{key}={value}\n")
    print(json.dumps(outputs, sort_keys=True))


if __name__ == "__main__":
    main()
