# Prebuilt github-actions-maintainer runtime image from this repository's
# docker.yml pipeline. Dependabot (docker ecosystem, /runtime directory) bumps
# this pin after each release; keep the tag in sync with the digest.
FROM ghcr.io/threatflux/github_actions:candidate-74115b390b69e28e743a73766d3ac50ef4a04a12@sha256:bb6ea66b5d6793ea5c5d490a0d37ee678865e1db52f965de53a93f30af8f514a

# Images built from the root Dockerfile are distroless Debian 13 (no shell)
# whose default user is nonroot (65532). GitHub Docker actions run with the
# workspace and runner file-command mounts owned by the runner user; run as
# root (the Docker-action default GitHub recommends) so reading the checkout
# and appending to GITHUB_OUTPUT work.
USER root
