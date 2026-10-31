# Prebuilt github-actions-maintainer runtime image from this repository's
# docker.yml pipeline. Dependabot (docker ecosystem, /release directory) bumps
# this pin after each release; keep the tag in sync with the digest.
FROM ghcr.io/threatflux/github_actions:0.7.9@sha256:2a4004cc0e605d6ff8898c418ff6c2b86c5934fd2da08e8609075fd8f60c257a

# Images built from the root Dockerfile are distroless Debian 13 (no shell)
# whose default user is nonroot (65532). GitHub Docker actions run with the
# workspace and runner file-command mounts owned by the runner user; run as
# root (the Docker-action default GitHub recommends) so reading the checkout
# and appending to GITHUB_OUTPUT work.
USER root
