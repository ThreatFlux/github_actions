#!/usr/bin/env python3
"""Resolve this repository's release source to a commit in fetched main history."""

import argparse
import os
import re
import subprocess
from pathlib import Path

LOCAL_GIT_ENV = {
    "GIT_ALTERNATE_OBJECT_DIRECTORIES", "GIT_COMMON_DIR", "GIT_DIR",
    "GIT_GRAFT_FILE", "GIT_IMPLICIT_WORK_TREE", "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY", "GIT_PREFIX", "GIT_WORK_TREE",
}


def git(root: Path, *args: str) -> str:
    """Keep fixture and release repositories independent of hook-local Git state."""
    env = {
        key: value for key, value in os.environ.items()
        if key not in LOCAL_GIT_ENV and not key.startswith("GIT_CONFIG")
    }
    result = subprocess.run(
        ["git", "-C", str(root), *args], env=env, check=True,
        text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    return result.stdout.strip()


def candidates(source: str) -> list[str]:
    if re.fullmatch(r"[0-9a-f]{40}", source):
        return [source]
    if not source or source.startswith("-"):
        raise ValueError("release source must be a branch, tag, or full commit SHA")
    if source.startswith(("refs/heads/", "refs/tags/")):
        return [source.replace("refs/heads/", "refs/remotes/origin/", 1)]
    if source.startswith("refs/"):
        raise ValueError("release sources may only use branch and tag namespaces")
    return [f"refs/remotes/origin/{source}", f"refs/tags/{source}"]


def resolve_release_source(root: Path, source: str) -> str:
    matches = set()
    for ref in candidates(source):
        if not re.fullmatch(r"[0-9a-f]{40}", ref):
            git(root, "check-ref-format", ref)
        try:
            matches.add(git(root, "rev-parse", "--verify", f"{ref}^{{commit}}"))
        except subprocess.CalledProcessError:
            continue
    if len(matches) != 1:
        raise ValueError("release source is missing or resolves ambiguously")
    revision = matches.pop()
    main = git(root, "rev-parse", "--verify", "refs/remotes/origin/main^{commit}")
    if not re.fullmatch(r"[0-9a-f]{40}", revision):
        raise ValueError("release source did not resolve to a complete commit SHA")
    git(root, "merge-base", "--is-ancestor", revision, main)
    return revision


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source")
    parser.add_argument("--repo", type=Path, default=Path.cwd())
    args = parser.parse_args()
    print(resolve_release_source(args.repo, args.source))


if __name__ == "__main__":
    main()
