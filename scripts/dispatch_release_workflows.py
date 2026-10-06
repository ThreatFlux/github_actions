#!/usr/bin/env python3
"""Dispatch, or deliberately skip, the downstream workflows of reusable-auto-release.yml.

Which token created the release tag decides whether anything has to be
dispatched:

- ``github-token``: GitHub suppresses workflow triggers for events made with
  the workflow's own ``GITHUB_TOKEN``, so the tag started nothing and every
  workflow in ``dispatch-workflows`` must be dispatched on it.
- ``github-app``: a tag pushed with a GitHub App installation token is an
  ordinary ``push`` event, so the caller's ``on: push: tags`` workflows
  already started. Dispatching them as well would run each one twice, so the
  dispatch is skipped unless the caller set ``dispatch-on-app-token`` for
  workflows that have no tag trigger.
- ``release-token``: dispatches, unchanged from earlier releases. A caller
  whose ``release-token`` is itself a PAT or App token, and whose workflows
  trigger on tags, should leave ``dispatch-workflows`` empty.

The workflow passes everything through environment variables. ``--plan-only``
prints the decision without running anything; the reusable workflow uses it
for dry runs.
"""

from __future__ import annotations

import argparse
import os

# B404: gh runs as an argv list built from validated inputs, never via a shell.
import subprocess  # nosec B404
from dataclasses import dataclass

RELEASERS = ("github-app", "release-token", "github-token")


@dataclass(frozen=True)
class Release:
    """What was released, how, and what the caller asked to dispatch."""

    repository: str
    tag: str
    version: str
    workflows: tuple[str, ...]
    version_workflow: str
    released_by: str
    dispatch_for_app_release: bool


@dataclass(frozen=True)
class Plan:
    """The gh commands to run, or the reason none run."""

    commands: tuple[tuple[str, ...], ...]
    skip_reason: str = ""


def parse_workflows(value: str) -> tuple[str, ...]:
    workflows = []
    for item in value.split(","):
        workflow = item.strip()
        if not workflow:
            continue
        if workflow.startswith("-"):
            raise ValueError(f"invalid workflow name {workflow!r}")
        workflows.append(workflow)
    return tuple(workflows)


def plan_dispatch(release: Release) -> Plan:
    """Decide which ``gh workflow run`` commands the release needs."""
    if release.released_by not in RELEASERS:
        raise ValueError(f"unknown releaser {release.released_by!r}; expected one of {', '.join(RELEASERS)}")
    if not release.workflows:
        return Plan((), "dispatch-workflows is empty")
    if release.released_by == "github-app" and not release.dispatch_for_app_release:
        return Plan((), (
            f"the GitHub App token created {release.tag}, and a tag pushed by an App starts the repository's"
            " `on: push: tags` workflows by itself; dispatching "
            + ", ".join(release.workflows)
            + " as well would run each twice. Set dispatch-on-app-token: true for workflows without a tag trigger."
        ))
    commands = []
    for workflow in release.workflows:
        command = ["gh", "workflow", "run", workflow, "--repo", release.repository, "--ref", release.tag]
        if workflow == release.version_workflow:
            command += ["-f", f"version={release.version}"]
        commands.append(tuple(command))
    return Plan(tuple(commands))


def append(path: str, text: str) -> None:
    if path:
        with open(path, "a", encoding="utf-8") as file:
            file.write(text)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    parser.add_argument("--plan-only", action="store_true", help="print the decision; run nothing")
    args = parser.parse_args(argv)

    env = os.environ
    tag = env.get("TAG", "")
    released_by = env.get("RELEASED_BY", "")
    workflows = parse_workflows(env.get("DISPATCH_WORKFLOWS", ""))
    if args.plan_only and not tag:
        print("Dry run: no release would be cut, so no workflow would be dispatched.")
        return 0
    if not tag:
        raise SystemExit("TAG is empty; refusing to dispatch workflows without a release tag")

    plan = plan_dispatch(Release(
        repository=env["GITHUB_REPOSITORY"],
        tag=tag,
        version=env.get("VERSION", ""),
        workflows=workflows,
        version_workflow=env.get("VERSION_WORKFLOW", "").strip(),
        released_by=released_by,
        dispatch_for_app_release=env.get("DISPATCH_FOR_APP_RELEASE", "false").strip().lower() == "true",
    ))
    if args.plan_only:
        print(f"Dry run: if the real run releases {tag}, it does so with the {released_by} token.")
        prefix = "Dry run: would"
    else:
        print(f"Release {tag} was made with the {released_by} token.")
        prefix = "Will"
    if plan.skip_reason:
        if args.plan_only:
            print(f"Dry run: would not dispatch: {plan.skip_reason}")
            return 0
        print(f"::notice title=Downstream dispatch skipped::Not dispatching: {plan.skip_reason}")
        append(env.get("GITHUB_STEP_SUMMARY", ""), f"Downstream dispatch skipped: {plan.skip_reason}\n")
        append(env.get("GITHUB_OUTPUT", ""), "dispatched=\n")
        return 0

    for command in plan.commands:
        print(f"{prefix} run: {' '.join(command)}")
        if not args.plan_only:
            # B603/B607: fixed gh subcommand from PATH; workflow names, repository, and tag are argv items.
            subprocess.run(command, check=True)  # nosec B603 B607
    dispatched = ",".join(command[3] for command in plan.commands)
    if not args.plan_only:
        append(env.get("GITHUB_STEP_SUMMARY", ""), f"Dispatched on {tag}: {dispatched}\n")
        append(env.get("GITHUB_OUTPUT", ""), f"dispatched={dispatched}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
