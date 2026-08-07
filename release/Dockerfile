# Prebuilt github-actions-maintainer runtime image from this repository's
# docker.yml pipeline. Dependabot (docker ecosystem, /release directory) bumps
# this pin after each release; keep the ref comment in sync with the digest.
#
# Bootstrap pin: main-branch build of 9c8f73c (v0.3.0 feature merge). Move to
# a semver tag + digest (ghcr.io/threatflux/github_actions:X.Y.Z@sha256:...)
# once tag-triggered image builds exist.
FROM ghcr.io/threatflux/github_actions@sha256:aac1d572318711698e9bd6a1e54ae0df0f3c6c4eab8257007048b5cf444573a5

# GitHub Docker actions run with the workspace and runner file-command mounts
# owned by the runner user; run as root (the Docker-action default GitHub
# recommends) so reading the checkout and appending to GITHUB_OUTPUT work.
USER root
