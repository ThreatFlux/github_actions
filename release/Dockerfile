# Prebuilt github-actions-maintainer runtime image from this repository's
# docker.yml pipeline. Dependabot (docker ecosystem, /release directory) bumps
# this pin after each release; keep the tag in sync with the digest.
FROM ghcr.io/threatflux/github_actions:0.7.0@sha256:c9d6e0fd9b8d444fe4d7fb3e2d0c2f106a3211fd8d28e94f189f5a503050e748

# GitHub Docker actions run with the workspace and runner file-command mounts
# owned by the runner user; run as root (the Docker-action default GitHub
# recommends) so reading the checkout and appending to GITHUB_OUTPUT work.
USER root
