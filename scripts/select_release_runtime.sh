#!/usr/bin/env bash
# Select a signed exact-head image, or a reviewed pin from the same build tree.
set -euo pipefail

repo_path="${GITHUB_REPOSITORY,,}"
image="ghcr.io/${repo_path}"

verify_image() {
  local ref="$1"
  cosign verify \
    --certificate-identity "https://github.com/${GITHUB_REPOSITORY}/.github/workflows/docker.yml@refs/heads/main" \
    --certificate-oidc-issuer https://token.actions.githubusercontent.com \
    "${ref}" >/dev/null || return
  docker pull "${ref}" >&2 || return
  docker image inspect --format '{{ index .Config.Labels "org.opencontainers.image.revision" }}' "${ref}"
}

digest="${HEAD_DIGEST}"
ready_to_tag=false
if [[ "${PHASE}" == tag ]]; then
  runtime_pin="$(awk '$1 == "FROM" { print $2; exit }' runtime/Dockerfile)"
  release_pin="$(awk '$1 == "FROM" { print $2; exit }' release/Dockerfile)"
  pinned_digest="${runtime_pin##*@}"
  if [[ "${pinned_digest}" =~ ^sha256:[a-f0-9]{64}$ &&
        "${runtime_pin}" == "${image}:${VERSION}@${pinned_digest}" &&
        "${release_pin}" == "${runtime_pin}" ]]; then
    revision="$(verify_image "${image}@${pinned_digest}")"
    if [[ "${revision}" =~ ^[a-f0-9]{40}$ ]] &&
       git merge-base --is-ancestor "${revision}" "${TARGET_SHA}" &&
       git diff --quiet "${revision}" "${TARGET_SHA}" -- . \
         ':(exclude)runtime/Dockerfile' ':(exclude)release/Dockerfile'; then
      digest="${pinned_digest}"
      ready_to_tag=true
    fi
  fi
fi

if [[ "${ready_to_tag}" != true ]]; then
  [[ "${digest}" =~ ^sha256:[a-f0-9]{64}$ ]]
  revision="$(verify_image "${image}@${digest}")"
  if [[ "${revision}" != "${TARGET_SHA}" ]]; then
    echo "runtime image revision ${revision} does not match validated main ${TARGET_SHA}" >&2
    exit 1
  fi
fi

reported="$(docker run --rm "${image}@${digest}" --version | awk '{ print $NF }')"
if [[ "${reported}" != "${VERSION}" ]]; then
  echo "runtime image ${digest} reports ${reported}, expected ${VERSION}" >&2
  exit 1
fi

{
  echo "digest=${digest}"
  echo "ready_to_tag=${ready_to_tag}"
} >> "${GITHUB_OUTPUT}"
