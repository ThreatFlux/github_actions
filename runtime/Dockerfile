# Prebuilt github-actions-maintainer runtime image from this repository's
# docker.yml pipeline. Dependabot (docker ecosystem, /runtime directory) bumps
# this pin after each release; keep the tag in sync with the digest.
FROM ghcr.io/threatflux/github_actions:0.7.10@sha256:6ec8e8edf77fa1e29b3a51514d7f7253eea29fd89053fd8ea22da848bd7c72c3

# Images built from the root Dockerfile are distroless Debian 13 (no shell)
# whose default user is nonroot (65532). GitHub Docker actions run with the
# workspace and runner file-command mounts owned by the runner user; run as
# root (the Docker-action default GitHub recommends) so reading the checkout
# and appending to GITHUB_OUTPUT work.
USER root
