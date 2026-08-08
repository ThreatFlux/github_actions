# Prebuilt github-actions-maintainer runtime image from this repository's
# docker.yml pipeline. Dependabot (docker ecosystem, /release directory) bumps
# this pin after each release; keep the tag in sync with the digest.
FROM ghcr.io/threatflux/github_actions:0.6.2@sha256:b29db5fd0b504b3007886dc3c7ddc8ecba0579271caaffaefe93cb925f0ce49d

# GitHub Docker actions run with the workspace and runner file-command mounts
# owned by the runner user; run as root (the Docker-action default GitHub
# recommends) so reading the checkout and appending to GITHUB_OUTPUT work.
USER root
