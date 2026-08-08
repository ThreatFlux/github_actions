# Prebuilt github-actions-maintainer runtime image from this repository's
# docker.yml pipeline. Dependabot (docker ecosystem, /release directory) bumps
# this pin after each release; keep the tag in sync with the digest.
FROM ghcr.io/threatflux/github_actions:0.6.0@sha256:60f61a1b91bee436c1a61d34773ae08f4a126f98c80707f52f4f9af6127f288f

# GitHub Docker actions run with the workspace and runner file-command mounts
# owned by the runner user; run as root (the Docker-action default GitHub
# recommends) so reading the checkout and appending to GITHUB_OUTPUT work.
USER root
