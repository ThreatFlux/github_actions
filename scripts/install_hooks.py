#!/usr/bin/env python3
"""Install hooks for this worktree without replacing another checkout's hooks."""

import subprocess
from pathlib import Path


def main() -> None:
    def git(*args: str) -> str:
        return subprocess.check_output(["git", *args], text=True).strip()

    git_dir = Path(git("rev-parse", "--absolute-git-dir"))
    common_dir = Path(git("rev-parse", "--git-common-dir")).resolve()
    hooks = git_dir / "hooks"
    if git_dir != common_dir:
        git("config", "extensions.worktreeConfig", "true")
        git("config", "--worktree", "core.hooksPath", str(hooks))
    else:
        hooks = Path(git("rev-parse", "--git-path", "hooks"))
    hooks.mkdir(parents=True, exist_ok=True)
    hook = hooks / "pre-commit"
    hook.write_text("#!/bin/sh\nset -eu\nexec make pre-commit\n")
    hook.chmod(0o755)


if __name__ == "__main__":
    main()
