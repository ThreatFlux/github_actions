#!/usr/bin/env bash
# Rotate the threatflux-automation GitHub App private key stored in the
# TF_AUTOMATION_APP_PRIVATE_KEY organization secret.
#
# GitHub has no API that creates App private keys, so a person generates the
# new key in the App's settings ("Generate a private key") and hands the
# downloaded .pem to this script, which:
#
#   1. checks the file is an unencrypted RSA private key, without printing it;
#   2. proves the key belongs to the App by signing a JWT and calling GET /app;
#   3. checks that the secret and the TF_AUTOMATION_APP_ID variable are shared
#      with exactly the repositories in .github/automation-app-repos.txt, and
#      stops on any difference unless --sync-repos is given;
#   4. replaces the secret, shared with exactly those repositories;
#   5. records the new key's SHA256 fingerprint and the rotation time in the
#      TF_AUTOMATION_APP_KEY_FINGERPRINT and TF_AUTOMATION_APP_KEY_ROTATED_AT
#      organization variables, which automation-app-health.yml dates the key
#      by (the secret's own updated_at also moves when its access changes);
#   6. dispatches automation-app-health.yml and waits for it to pass, which
#      proves Actions can mint installation tokens with the new key;
#   7. prints where to delete the OLD key, then securely removes the local .pem.
#
# --check-repos runs step 3 alone and needs no key.
#
# Key material never reaches the terminal, a command line, or a log: the key
# is only ever read through file redirection. Runbook: docs/SECRETS-ROTATION.md.
#
# Requirements: gh (authenticated as an organization owner, admin:org scope),
# jq, curl, and openssl.
set -euo pipefail
umask 077

ORG="ThreatFlux"
SECRET_NAME="TF_AUTOMATION_APP_PRIVATE_KEY"
APP_ID_VARIABLE="TF_AUTOMATION_APP_ID"
FINGERPRINT_VARIABLE="TF_AUTOMATION_APP_KEY_FINGERPRINT"
ROTATED_AT_VARIABLE="TF_AUTOMATION_APP_KEY_ROTATED_AT"
HEALTH_REPO="ThreatFlux/github_actions"
HEALTH_WORKFLOW="automation-app-health.yml"
HEALTH_REF="main"
API_URL="${GITHUB_API_URL:-https://api.github.com}"
WEB_URL="${GITHUB_SERVER_URL:-https://github.com}"
POLL_SECONDS="${ROTATE_POLL_SECONDS:-5}"
POLL_ATTEMPTS="${ROTATE_POLL_ATTEMPTS:-36}"
REPOS_FILE_DEFAULT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/.github/automation-app-repos.txt"

dry_run=false
assume_yes=false
check_only=false
sync_repos=false
app_id=""
repos_file="${REPOS_FILE_DEFAULT}"
pem=""

usage() {
    cat <<USAGE
Usage: ${0##*/} [--dry-run] [--yes] [--sync-repos] [--app-id ID] [--repos-file FILE] NEW_KEY.pem
       ${0##*/} --check-repos [--repos-file FILE]

Replace the ${ORG} organization secret ${SECRET_NAME} with a newly generated
GitHub App private key, record its fingerprint and rotation time, prove it
works, then remove the local key file.

  --dry-run          Validate the key and show the plan; change nothing and keep the file.
  --yes              Do not ask for confirmation before replacing the secret.
  --sync-repos       Share ${SECRET_NAME} and ${APP_ID_VARIABLE} with exactly the
                     repositories in the list even when they differ from it today
                     (otherwise any difference stops the script before it changes anything).
  --check-repos      Only compare ${SECRET_NAME} and ${APP_ID_VARIABLE} with the list;
                     exit 1 on any difference. Needs no key.
  --app-id ID        App ID to verify the key against (default: the ${APP_ID_VARIABLE}
                     organization variable).
  --repos-file FILE  Repository list (default: .github/automation-app-repos.txt).
  -h, --help         Show this help.
USAGE
}

log() { printf '==> %s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

while [[ $# -gt 0 ]]; do
    case "$1" in
        --dry-run) dry_run=true ;;
        --yes | -y) assume_yes=true ;;
        --sync-repos) sync_repos=true ;;
        --check-repos) check_only=true ;;
        --app-id) [[ $# -ge 2 ]] || die "--app-id needs a value"; app_id="$2"; shift ;;
        --repos-file) [[ $# -ge 2 ]] || die "--repos-file needs a value"; repos_file="$2"; shift ;;
        -h | --help) usage; exit 0 ;;
        --) shift; break ;;
        -*) usage >&2; die "unknown option: $1" ;;
        *) [[ -z "${pem}" ]] || die "only one key file may be given"; pem="$1" ;;
    esac
    shift
done
if [[ $# -gt 0 ]]; then
    [[ -z "${pem}" && $# -eq 1 ]] || die "only one key file may be given"
    pem="$1"
fi
if [[ "${check_only}" == true ]]; then
    [[ -z "${pem}" ]] || die "--check-repos takes no key file"
    required_tools=(gh)
else
    [[ -n "${pem}" ]] || { usage >&2; die "missing the path to the new .pem file"; }
    required_tools=(gh jq curl openssl)
fi

for tool in "${required_tools[@]}"; do
    command -v "${tool}" >/dev/null 2>&1 || die "${tool} is required"
done

# The checked-in repository list: names only, one per line, '#' comments.
# Prints the names sorted and comma-joined.
read_repo_list() {
    local file="$1" line name
    local listed=()
    [[ -f "${file}" ]] || die "repository list ${file} does not exist"
    while IFS= read -r line || [[ -n "${line}" ]]; do
        name="${line%%#*}"
        name="${name//[[:space:]]/}"
        [[ -n "${name}" ]] || continue
        [[ "${name}" =~ ^[A-Za-z0-9._-]+$ ]] || die "invalid repository name '${name}' in ${file}"
        listed+=("${name}")
    done < "${file}"
    [[ ${#listed[@]} -gt 0 ]] || die "${file} lists no repository"
    printf '%s\n' "${listed[@]}" | sort -fu | paste -sd, -
}

# Lower-cased, one per line, for comparisons: GitHub names are case-insensitive.
list_lines() { tr ',' '\n' <<<"$1" | tr '[:upper:]' '[:lower:]' | sed '/^$/d' | sort -u; }

# Visibility of an organization secret or variable ("secrets"/"variables"),
# or "missing" when it does not exist.
org_visibility() {
    local output
    if output="$(gh api "/orgs/${ORG}/actions/$1/$2" --jq .visibility 2>&1)"; then
        printf '%s\n' "${output}"
    elif grep -q 'HTTP 404' <<<"${output}"; then
        printf 'missing\n'
    else
        printf 'error: could not read %s/%s: %s\n' "${ORG}" "$2" "${output}" >&2
        return 1
    fi
}

org_repos() {
    gh api --paginate "/orgs/${ORG}/actions/$1/$2/repositories" --jq '.repositories[].name' | sort -f | paste -sd, -
}

# Compare one secret's or variable's access with the list; returns 1 on drift.
check_access() {
    local kind="$1" name="$2" visibility="$3" actual="$4" missing unlisted
    if [[ "${visibility}" == missing ]]; then
        warn "${name} does not exist as an organization ${kind%s}"
        return 1
    fi
    if [[ "${visibility}" != selected ]]; then
        warn "${name} is visible to ${visibility} repositories, not only the ${expected_count} listed ones"
        return 1
    fi
    missing="$(comm -23 <(list_lines "${expected}") <(list_lines "${actual}") | paste -sd, -)"
    unlisted="$(comm -13 <(list_lines "${expected}") <(list_lines "${actual}") | paste -sd, -)"
    if [[ -z "${missing}" && -z "${unlisted}" ]]; then
        log "${name} is shared with exactly the ${expected_count} listed repositories"
        return 0
    fi
    [[ -z "${missing}" ]] || warn "${name} is not shared with listed repositories: ${missing}"
    [[ -z "${unlisted}" ]] || warn "${name} is shared with repositories that are not listed: ${unlisted}"
    return 1
}

# --- 3 (first, it needs no key). Repository access matches the list ---------
expected="$(read_repo_list "${repos_file}")"
expected_count="$(list_lines "${expected}" | wc -l | tr -d ' ')"
log "Repository list ${repos_file}: ${expected_count} repositories"

# A failed lookup or listing stops the script here (errexit), before anything
# is compared or changed, so an API error is never mistaken for drift.
secret_visibility="$(org_visibility secrets "${SECRET_NAME}")"
secret_repos=""
if [[ "${secret_visibility}" == selected ]]; then
    secret_repos="$(org_repos secrets "${SECRET_NAME}")"
fi
variable_visibility="$(org_visibility variables "${APP_ID_VARIABLE}")"
variable_repos=""
if [[ "${variable_visibility}" == selected ]]; then
    variable_repos="$(org_repos variables "${APP_ID_VARIABLE}")"
fi

secret_drift=false
variable_drift=false
check_access secrets "${SECRET_NAME}" "${secret_visibility}" "${secret_repos}" || secret_drift=true
check_access variables "${APP_ID_VARIABLE}" "${variable_visibility}" "${variable_repos}" || variable_drift=true

if [[ "${check_only}" == true ]]; then
    if [[ "${secret_drift}" == true || "${variable_drift}" == true ]]; then
        die "repository access differs from ${repos_file}; the installation is checked by ${HEALTH_WORKFLOW}"
    fi
    log "${SECRET_NAME} and ${APP_ID_VARIABLE} match ${repos_file}; the installation is checked by ${HEALTH_WORKFLOW}"
    exit 0
fi

if [[ "${sync_repos}" != true ]]; then
    # A missing secret is simply created for the listed repositories.
    if [[ "${secret_visibility}" != missing && "${secret_drift}" == true ]] || [[ "${variable_drift}" == true ]]; then
        die "repository access differs from ${repos_file}; fix the list (in a pull request) or the settings, or rerun with --sync-repos to share both with exactly the listed repositories. Nothing changed"
    fi
fi

# --- 1. The file must be an unencrypted RSA private key ----------------------
[[ -f "${pem}" && ! -L "${pem}" ]] || die "${pem} is not a regular file"
[[ -r "${pem}" ]] || die "${pem} is not readable"
grep -q -- '-----BEGIN [A-Z ]*PRIVATE KEY-----' "${pem}" || die "${pem} is not a PEM private key"
if grep -q -- 'ENCRYPTED' "${pem}"; then
    die "${pem} is encrypted; use the unencrypted key GitHub downloads"
fi
# -check -noout prints only "RSA key ok"; an empty passphrase makes an
# encrypted key fail instead of prompting.
openssl rsa -in "${pem}" -passin pass: -check -noout >/dev/null 2>&1 \
    || die "${pem} is not a valid unencrypted RSA private key"
# The format GitHub shows for each key in the App's settings.
fingerprint="SHA256:$(openssl rsa -in "${pem}" -passin pass: -pubout -outform DER 2>/dev/null \
    | openssl dgst -sha256 -binary | openssl base64 -A)"
log "New key is a valid RSA private key, fingerprint ${fingerprint}"

# --- 2. The key must belong to the App ---------------------------------------
if [[ -z "${app_id}" ]]; then
    app_id="$(gh api "/orgs/${ORG}/actions/variables/${APP_ID_VARIABLE}" --jq .value)" \
        || die "could not read the ${APP_ID_VARIABLE} organization variable; pass --app-id"
fi
[[ "${app_id}" =~ ^[0-9]+$ ]] || die "App ID '${app_id}' is not numeric"

base64url() { openssl base64 -A | tr '+/' '-_' | tr -d '='; }
now="$(date +%s)"
jwt_header="$(printf '{"alg":"RS256","typ":"JWT"}' | base64url)"
jwt_claims="$(printf '{"iat":%d,"exp":%d,"iss":"%s"}' "$((now - 60))" "$((now + 300))" "${app_id}" | base64url)"
jwt_signature="$(printf '%s.%s' "${jwt_header}" "${jwt_claims}" \
    | openssl dgst -sha256 -sign "${pem}" -passin pass: -binary | base64url)"
# The JWT can mint installation tokens, so it travels on stdin, never argv.
response="$(printf 'Authorization: Bearer %s.%s.%s\n' "${jwt_header}" "${jwt_claims}" "${jwt_signature}" \
    | curl -sS -H @- -H 'Accept: application/vnd.github+json' -H 'X-GitHub-Api-Version: 2022-11-28' \
        -w '\n%{http_code}' "${API_URL}/app")" || die "could not reach ${API_URL}/app"
unset jwt_signature
http_status="${response##*$'\n'}"
app_json="${response%$'\n'*}"
if [[ "${http_status}" != 200 ]]; then
    die "GitHub rejected a JWT signed with this key for App ${app_id} (HTTP ${http_status}: $(jq -r '.message // empty' <<<"${app_json}" 2>/dev/null)); is it a key of that App?"
fi
returned_id="$(jq -r '.id // empty' <<<"${app_json}")"
app_slug="$(jq -r '.slug // empty' <<<"${app_json}")"
[[ "${returned_id}" == "${app_id}" ]] || die "the key authenticates App '${returned_id}', not ${app_id}"
log "Key authenticates App ${app_id} (${app_slug})"

repos="${expected}"
log "Secret ${SECRET_NAME} will be shared with: ${repos}"

# A repository secret with the same name overrides the organization secret.
# It would keep the old key in use, and keep health runs green with it, so
# deleting the old key afterwards would break that repository.
shadowed=""
IFS=, read -ra repo_list <<<"${repos}"
for repo in "${repo_list[@]}"; do
    names="$(gh api --paginate "/repos/${ORG}/${repo}/actions/secrets" --jq '.secrets[].name')" \
        || die "could not list ${ORG}/${repo} repository secrets to rule out a ${SECRET_NAME} that overrides the org secret"
    if grep -qx -- "${SECRET_NAME}" <<<"${names}"; then
        shadowed="${shadowed:+${shadowed}, }${repo}"
    fi
done
[[ -z "${shadowed}" ]] \
    || die "a repository secret named ${SECRET_NAME} overrides the organization secret in: ${shadowed}; delete it first"
log "No repository secret overrides ${SECRET_NAME}"

settings_url="${WEB_URL}/organizations/${ORG}/settings/apps/${app_slug:-threatflux-automation}"
record_scope=(--org "${ORG}" --visibility selected --repos "${HEALTH_REPO#*/}")

if [[ "${dry_run}" == true ]]; then
    if [[ "${variable_drift}" == true ]]; then
        log "Dry run: would run gh variable set ${APP_ID_VARIABLE} --org ${ORG} --visibility selected --repos ${repos} --body ${app_id}"
    fi
    log "Dry run: would run gh secret set ${SECRET_NAME} --org ${ORG} --visibility selected --repos ${repos} < ${pem}"
    log "Dry run: would record ${ROTATED_AT_VARIABLE}=<rotation time, UTC> and ${FINGERPRINT_VARIABLE}=${fingerprint} (organization variables visible to ${HEALTH_REPO#*/})"
    log "Dry run: would dispatch ${HEALTH_WORKFLOW} on ${HEALTH_REPO}@${HEALTH_REF} and wait for it to pass"
    log "Dry run: would then remove ${pem} and point at ${settings_url} to delete the old key"
    log "Dry run: nothing changed; ${pem} was kept"
    exit 0
fi

if [[ "${assume_yes}" != true ]]; then
    [[ -t 0 ]] || die "refusing to replace the secret without confirmation; rerun with --yes"
    read -r -p "Replace ${ORG}/${SECRET_NAME} with the key ${fingerprint}? [y/N] " answer
    [[ "${answer}" =~ ^[Yy]([Ee][Ss])?$ ]] || die "aborted; nothing changed"
fi

# --sync-repos: the App ID variable needs no key, so it is fixed first and a
# failure leaves the secret untouched.
if [[ "${variable_drift}" == true ]]; then
    gh variable set "${APP_ID_VARIABLE}" --org "${ORG}" --visibility selected --repos "${repos}" --body "${app_id}" \
        || die "gh variable set ${APP_ID_VARIABLE} failed; the secret was not changed and ${pem} was kept"
    log "Shared ${ORG}/${APP_ID_VARIABLE} with exactly the listed repositories"
fi

# --- 4. Replace the secret ----------------------------------------------------
gh secret set "${SECRET_NAME}" --org "${ORG}" --visibility selected --repos "${repos}" < "${pem}" \
    || die "gh secret set failed; the secret was not changed and ${pem} was kept"
log "Updated ${ORG}/${SECRET_NAME}"

# --- 5. Record which key is stored and since when -----------------------------
# The time goes first: if the fingerprint then fails to update, the record
# names the old key, the health run sees the mismatch and falls back to the
# secret's updated_at instead of trusting a stale date.
rotated_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
if gh variable set "${ROTATED_AT_VARIABLE}" "${record_scope[@]}" --body "${rotated_at}" >/dev/null \
    && gh variable set "${FINGERPRINT_VARIABLE}" "${record_scope[@]}" --body "${fingerprint}" >/dev/null; then
    log "Recorded ${FINGERPRINT_VARIABLE}=${fingerprint} and ${ROTATED_AT_VARIABLE}=${rotated_at}"
else
    warn "could not record the rotation; ${HEALTH_WORKFLOW} will date the key by the secret's last update until you run:"
    warn "  gh variable set ${ROTATED_AT_VARIABLE} ${record_scope[*]} --body '${rotated_at}'"
    warn "  gh variable set ${FINGERPRINT_VARIABLE} ${record_scope[*]} --body '${fingerprint}'"
fi

# --- 6. Prove Actions can use it ------------------------------------------------
# The run-ID baseline is taken only after the secret was replaced: run IDs
# only grow, so every run above it was created later and reads the new key,
# even if the same person dispatches another run at the same time.
actor="$(gh api /user --jq .login)"
last_run="$(gh run list --repo "${HEALTH_REPO}" --workflow "${HEALTH_WORKFLOW}" --limit 1 \
    --json databaseId --jq '.[0].databaseId // 0')"
[[ "${last_run}" =~ ^[0-9]+$ ]] || die "could not read the latest ${HEALTH_WORKFLOW} run; ${pem} was kept"
gh workflow run "${HEALTH_WORKFLOW}" --repo "${HEALTH_REPO}" --ref "${HEALTH_REF}" >/dev/null
log "Dispatched ${HEALTH_WORKFLOW}; waiting for the run to start"
run_id=""
for _ in $(seq 1 "${POLL_ATTEMPTS}"); do
    run_id="$(gh run list --repo "${HEALTH_REPO}" --workflow "${HEALTH_WORKFLOW}" --event workflow_dispatch \
        --user "${actor}" --limit 10 --json databaseId \
        --jq "[.[] | select(.databaseId > ${last_run})] | min_by(.databaseId) | .databaseId // empty")"
    [[ -n "${run_id}" ]] && break
    sleep "${POLL_SECONDS}"
done
if [[ -z "${run_id}" ]]; then
    warn "could not find the dispatched ${HEALTH_WORKFLOW} run; check ${WEB_URL}/${HEALTH_REPO}/actions"
    die "the new key is stored but unverified; ${pem} was kept, and the old key must stay until a health run passes"
fi
run_url="${WEB_URL}/${HEALTH_REPO}/actions/runs/${run_id}"
log "Watching ${run_url}"
if ! gh run watch "${run_id}" --repo "${HEALTH_REPO}" --exit-status --interval 10 >/dev/null; then
    die "the health run failed: ${run_url}. The new key is stored but did not work; keep the old key, fix the cause, and rerun. ${pem} was kept"
fi
log "Health run passed: Actions mints installation tokens with the new key"

# --- 7. Retire the old key and the local file ---------------------------------
cat <<NEXT

Next step (manual, GitHub has no API for it): delete the OLD private key.

  1. Open ${settings_url}
  2. Under "Private keys", keep the key with fingerprint
       ${fingerprint}
     and delete every other key.
  3. Optionally dispatch ${HEALTH_WORKFLOW} again; it must still pass.

NEXT

case "$(uname -s)" in
    Darwin) rm -P -- "${pem}" ;;
    Linux)
        if command -v shred >/dev/null 2>&1; then
            shred -u -z -- "${pem}"
        else
            rm -f -- "${pem}"
        fi
        ;;
    *) rm -f -- "${pem}" ;;
esac
[[ ! -e "${pem}" ]] || die "could not remove ${pem}; delete it by hand"
log "Removed ${pem}. Also empty it from Downloads and any trash or backup it reached."
