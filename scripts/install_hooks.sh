#!/bin/sh
# Install hooks in this worktree without replacing another checkout's hooks.
set -eu

git_dir="$(git rev-parse --absolute-git-dir)"
common_dir="$(git rev-parse --path-format=absolute --git-common-dir)"
hooks="${git_dir}/hooks"
if [ "${git_dir}" != "${common_dir}" ]; then
    git config extensions.worktreeConfig true
    git config --worktree core.hooksPath "${hooks}"
else
    hooks="$(git rev-parse --git-path hooks)"
fi

mkdir -p "${hooks}"
cat > "${hooks}/pre-commit" <<'HOOK'
#!/bin/sh
set -eu
exec make pre-commit
HOOK
chmod +x "${hooks}/pre-commit"
