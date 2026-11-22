# Prebuilt github-actions-maintainer runtime image from this repository's
# docker.yml pipeline. Dependabot (docker ecosystem, /runtime directory) bumps
# this pin after each release; keep the tag in sync with the digest.
FROM ghcr.io/threatflux/github_actions:candidate-35c33050f329b7c93b49b027b020c3fce2016c52@sha256:48f202272793e774ee68385f3ee03ecdd0f293fa8711263df6630acaac54b429

# Images built from the root Dockerfile are distroless Debian 13 (no shell)
# whose default user is nonroot (65532). GitHub Docker actions run with the
# workspace and runner file-command mounts owned by the runner user; run as
# root (the Docker-action default GitHub recommends) so reading the checkout
# and appending to GITHUB_OUTPUT work.
USER root
