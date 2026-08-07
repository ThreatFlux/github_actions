# Prebuilt github-actions-maintainer runtime image from this repository's
# docker.yml pipeline. Dependabot (docker ecosystem, /release directory) bumps
# this pin after each release; keep the tag in sync with the digest.
FROM ghcr.io/threatflux/github_actions:0.4.0@sha256:848419919249e253e7ad79b05479496e5be15adebbeb05983c00a555d3662a8a

# GitHub Docker actions run with the workspace and runner file-command mounts
# owned by the runner user; run as root (the Docker-action default GitHub
# recommends) so reading the checkout and appending to GITHUB_OUTPUT work.
USER root
