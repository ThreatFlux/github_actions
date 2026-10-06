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

``--plan-only`` prints the decision without running anything; the reusable
workflow uses it for dry runs.
"""

from __future__ import annotations

import argparse
import os

# B404: gh runs as an argv list built from validated inputs, never via a shell.
import subprocess  # nosec B404
from dataclasses import dataclass

RELEASERS = {
    "github-app": "the GitHub App token",
    "release-token": "the release-token secret",
    "github-token": "the default GITHUB_TOKEN",
}


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
    """The ``gh workflow run`` arguments to use, or the reason none run."""

    runs: tuple[tuple[str, ...], ...]
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
    """Decide which ``gh workflow run`` invocations the release needs (arguments after ``run``)."""
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
    runs = []
    for workflow in release.workflows:
        run = [workflow, "--repo", release.repository, "--ref", release.tag]
        if workflow == release.version_workflow:
            run += ["-f", f"version={release.version}"]
        runs.append(tuple(run))
    return Plan(tuple(runs))


def append(path: str, text: str) -> None:
    if path:
        with open(path, "a", encoding="utf-8") as file:
            file.write(text)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    parser.add_argument("--repository", required=True, help="OWNER/REPO the workflows belong to")
    parser.add_argument("--tag", default="", help="the release tag; empty when nothing was released")
    parser.add_argument("--version", default="", help="the released version, passed to --version-workflow")
    parser.add_argument("--workflows", default="", help="comma-separated workflow files (dispatch-workflows)")
    parser.add_argument("--version-workflow", default="", help="the workflow that takes a version input")
    parser.add_argument("--released-by", required=True, choices=tuple(RELEASERS))
    parser.add_argument("--dispatch-for-app-release", default="false",
                        help="dispatch-on-app-token: 'true' also dispatches after a GitHub App release")
    parser.add_argument("--plan-only", action="store_true", help="print the decision; run nothing")
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    if args.plan_only and not args.tag:
        print("Dry run: no release would be cut, so no workflow would be dispatched.")
        return 0
    if not args.tag:
        raise SystemExit("--tag is empty; refusing to dispatch workflows without a release tag")

    plan = plan_dispatch(Release(
        repository=args.repository,
        tag=args.tag,
        version=args.version,
        workflows=parse_workflows(args.workflows),
        version_workflow=args.version_workflow.strip(),
        released_by=args.released_by,
        dispatch_for_app_release=args.dispatch_for_app_release.strip().lower() == "true",
    ))
    releaser = RELEASERS[args.released_by]
    summary = os.environ.get("GITHUB_STEP_SUMMARY", "")
    output = os.environ.get("GITHUB_OUTPUT", "")
    if args.plan_only:
        print(f"Dry run: if the real run releases {args.tag}, {releaser} creates it.")
    else:
        print(f"{releaser[0].upper()}{releaser[1:]} created {args.tag}.")

    if plan.skip_reason:
        if args.plan_only:
            print(f"Dry run: would not dispatch: {plan.skip_reason}")
            return 0
        print(f"::notice title=Downstream dispatch skipped::Not dispatching: {plan.skip_reason}")
        append(summary, f"Downstream dispatch skipped: {plan.skip_reason}\n")
        append(output, "dispatched=\n")
        return 0

    for run in plan.runs:
        print(f"{'Dry run: would run' if args.plan_only else 'Running'}: gh workflow run {' '.join(run)}")
        if not args.plan_only:
            # B603/B607: gh from PATH with a fixed subcommand; every other item is a single argv entry.
            subprocess.run(["gh", "workflow", "run", *run], check=True)  # nosec B603 B607
    if not args.plan_only:
        dispatched = ",".join(run[0] for run in plan.runs)
        append(summary, f"Dispatched on {args.tag}: {dispatched}\n")
        append(output, f"dispatched={dispatched}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
