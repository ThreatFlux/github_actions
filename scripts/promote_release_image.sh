#!/usr/bin/env bash
# Promote only the runtime digest sealed into an already-created release tag.
# This also repairs a post-tag registry failure without using current main.
set -euo pipefail

: "${VERSION:?Set the released version without v}"
: "${GITHUB_REPOSITORY:?Set owner/repository}"
[[ "${VERSION}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "invalid stable version" >&2; exit 1; }
tag="v${VERSION}"

read_pin() {
  gh api "repos/${GITHUB_REPOSITORY}/contents/$1?ref=${tag}" --jq .content \
    | base64 -d | awk '$1 == "FROM" { print $2; exit }'
}
runtime_pin="$(read_pin runtime/Dockerfile)"
release_pin="$(read_pin release/Dockerfile)"
image_ref="${runtime_pin%@*}"
image="${image_ref%:*}"
digest="${runtime_pin##*@}"
if [[ "${runtime_pin}" != "${release_pin}" ||
      "${image_ref}" != "ghcr.io/${GITHUB_REPOSITORY,,}:${VERSION}" ||
      ! "${digest}" =~ ^sha256:[a-f0-9]{64}$ ]]; then
  echo "the exact release tag must seal matching versioned runtime pins" >&2
  exit 1
fi
if [[ -n "${EXPECTED_IMAGE:-}" && "${EXPECTED_IMAGE}" != "${image}" ]] ||
   [[ -n "${EXPECTED_DIGEST:-}" && "${EXPECTED_DIGEST}" != "${digest}" ]]; then
  echo "sealed release runtime differs from the validated build" >&2
  exit 1
fi
manifest_version="$(gh api "repos/${GITHUB_REPOSITORY}/contents/Cargo.toml?ref=${tag}" --jq .content \
  | base64 -d | sed -n -E 's/^version = "([^"]+)"$/\1/p' | head -n 1)"
[[ "${manifest_version}" == "${VERSION}" ]] || { echo "tagged manifest version differs from release" >&2; exit 1; }
resolved="$(docker buildx imagetools inspect "${image}@${digest}" --format '{{.Manifest.Digest}}')"
reported="$(docker run --rm "${image}@${digest}" --version | awk '{ print $NF }')"
if [[ "${resolved}" != "${digest}" || "${reported}" != "${VERSION}" ]]; then
  echo "sealed runtime digest/version verification failed" >&2
  exit 1
fi
releases="$(gh api --paginate "repos/${GITHUB_REPOSITORY}/git/matching-refs/tags/v" \
  --jq '.[].ref | ltrimstr("refs/tags/v")' \
  | { grep -E '^[0-9]+\.[0-9]+\.[0-9]+$' || true; } | sort -V -r)"
if ! grep -Fxq -- "${VERSION}" <<< "${releases}"; then
  echo "release tag is not visible; retry recovery after it is available" >&2
  exit 1
fi
newest_release() {
  local prefix="$1" release
  while IFS= read -r release; do
    if [[ "${release}" == "${prefix}."* ]]; then
      echo "${release}"
      return
    fi
  done <<< "${releases}"
}
major="${VERSION%%.*}"
minor="${VERSION%.*}"
tags=("${VERSION}")
[[ "$(newest_release "${minor}")" != "${VERSION}" ]] || tags+=("${minor}")
[[ "$(newest_release "${major}")" != "${VERSION}" ]] || tags+=("${major}")
: "${GITHUB_TOKEN:?Set an authorized packages-write token}"
printf '%s\n' "${GITHUB_TOKEN}" | docker login ghcr.io -u "${GITHUB_ACTOR:?Set the authorized actor}" --password-stdin
arguments=()
for alias in "${tags[@]}"; do arguments+=(--tag "${image}:${alias}"); done
docker buildx imagetools create "${arguments[@]}" "${image}@${digest}"
for alias in "${tags[@]}"; do
  actual="$(docker buildx imagetools inspect "${image}:${alias}" --format '{{.Manifest.Digest}}')"
  [[ "${actual}" == "${digest}" ]] || { echo "published alias differs from sealed digest" >&2; exit 1; }
done
printf 'Published %s from %s: %s\n' "${image}" "${tag}" "${tags[*]}"
