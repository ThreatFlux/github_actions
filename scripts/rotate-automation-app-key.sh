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
#   3. reads which repositories the secret is shared with today;
#   4. replaces the secret, keeping exactly that repository list;
#   5. dispatches automation-app-health.yml and waits for it to pass, which
#      proves Actions can mint installation tokens with the new key;
#   6. prints where to delete the OLD key, then securely removes the local .pem.
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
HEALTH_REPO="ThreatFlux/github_actions"
HEALTH_WORKFLOW="automation-app-health.yml"
HEALTH_REF="main"
API_URL="${GITHUB_API_URL:-https://api.github.com}"
WEB_URL="${GITHUB_SERVER_URL:-https://github.com}"
POLL_SECONDS="${ROTATE_POLL_SECONDS:-5}"
POLL_ATTEMPTS="${ROTATE_POLL_ATTEMPTS:-36}"

dry_run=false
assume_yes=false
app_id=""
repos_override=""
pem=""

usage() {
    cat <<USAGE
Usage: ${0##*/} [--dry-run] [--yes] [--app-id ID] [--repos a,b,c] NEW_KEY.pem

Replace the ${ORG} organization secret ${SECRET_NAME} with a newly generated
GitHub App private key, prove it works, then remove the local key file.

  --dry-run     Validate the key and show the plan; change nothing and keep the file.
  --yes         Do not ask for confirmation before replacing the secret.
  --app-id ID   App ID to verify the key against (default: the ${APP_ID_VARIABLE}
                organization variable).
  --repos LIST  Comma-separated repositories to share the secret with, instead of
                the list the secret has today (needed only if the secret is missing).
  -h, --help    Show this help.
USAGE
}

log() { printf '==> %s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

while [[ $# -gt 0 ]]; do
    case "$1" in
        --dry-run) dry_run=true ;;
        --yes | -y) assume_yes=true ;;
        --app-id) [[ $# -ge 2 ]] || die "--app-id needs a value"; app_id="$2"; shift ;;
        --repos) [[ $# -ge 2 ]] || die "--repos needs a value"; repos_override="$2"; shift ;;
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
[[ -n "${pem}" ]] || { usage >&2; die "missing the path to the new .pem file"; }

for tool in gh jq curl openssl; do
    command -v "${tool}" >/dev/null 2>&1 || die "${tool} is required"
done

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

# --- 3. Keep the secret's current repository access --------------------------
if [[ -n "${repos_override}" ]]; then
    repos="$(tr -d '[:space:]' <<<"${repos_override}")"
else
    visibility="$(gh api "/orgs/${ORG}/actions/secrets/${SECRET_NAME}" --jq .visibility)" \
        || die "could not read ${ORG}/${SECRET_NAME}; pass --repos to create it"
    [[ "${visibility}" == selected ]] \
        || die "${SECRET_NAME} has visibility '${visibility}', expected 'selected'; pass --repos to reset it"
    repos="$(gh api --paginate "/orgs/${ORG}/actions/secrets/${SECRET_NAME}/repositories" \
        --jq '.repositories[].name' | sort | paste -sd, -)"
fi
[[ -n "${repos}" ]] || die "the secret would be shared with no repository"
[[ "${repos}" =~ ^[A-Za-z0-9._,-]+$ ]] || die "unexpected repository list '${repos}'"
log "Secret ${SECRET_NAME} stays shared with: ${repos}"

settings_url="${WEB_URL}/organizations/${ORG}/settings/apps/${app_slug:-threatflux-automation}"

if [[ "${dry_run}" == true ]]; then
    log "Dry run: would run gh secret set ${SECRET_NAME} --org ${ORG} --visibility selected --repos ${repos} < ${pem}"
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

# --- 4. Replace the secret ----------------------------------------------------
gh secret set "${SECRET_NAME}" --org "${ORG}" --visibility selected --repos "${repos}" < "${pem}" \
    || die "gh secret set failed; the secret was not changed and ${pem} was kept"
log "Updated ${ORG}/${SECRET_NAME}"

# --- 5. Prove Actions can use it ------------------------------------------------
actor="$(gh api /user --jq .login)"
last_run="$(gh run list --repo "${HEALTH_REPO}" --workflow "${HEALTH_WORKFLOW}" --limit 1 \
    --json databaseId --jq '.[0].databaseId // 0')"
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

# --- 6. Retire the old key and the local file ---------------------------------
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
