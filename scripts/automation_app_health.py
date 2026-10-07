#!/usr/bin/env python3
"""Turn the automation App health facts into findings, a report, and a status.

automation-app-health.yml gathers the raw facts with the App's own tokens: the
repositories the installation reaches, the repositories the App private key
secret and the App ID variable are shared with, the fingerprint of the stored
key, and (when the App holds the organization "Secrets: read" permission) the
metadata (names and dates, never values) of the organization's Actions
secrets. This script judges them against .github/automation-app-repos.txt:

- ``broken``: the App cannot mint a token, or the installation, the key secret
  or the App ID variable reaches other repositories than the checked-in list.
  The workflow fails.
- ``attention``: the App works, but a person has to act: a secret is due for
  rotation, an obsolete secret still exists, or a check could not run.
- ``healthy``: nothing to report, so the tracking issue is closed.

Notes (for example "no key rotation record yet") are reported but never change
the status.
"""

from __future__ import annotations

import argparse
import json
import os
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path

ERROR = "error"
WARNING = "warning"
NOTE = "note"
SEVERITY_ORDER = {ERROR: 0, WARNING: 1, NOTE: 2}

DEFAULT_KEY_MAX_AGE_DAYS = 90
DEFAULT_MAX_AGE_DAYS = 180
REPORT_MARKER = "<!-- automation-app-health -->"
RUNBOOK = "docs/SECRETS-ROTATION.md"
ROTATE_SCRIPT = "scripts/rotate-automation-app-key.sh"
REPOS_FILE = ".github/automation-app-repos.txt"
FINGERPRINT_VARIABLE = "TF_AUTOMATION_APP_KEY_FINGERPRINT"
ROTATED_AT_VARIABLE = "TF_AUTOMATION_APP_KEY_ROTATED_AT"

SECRETS_OUTCOMES = ("success", "missing-permission", "failure", "skipped")
VARIABLE_OUTCOMES = ("success", "missing-permission", "not-found", "failure", "skipped")

BASIS_RECORD = "rotation record"
BASIS_UPDATED = "secret last updated"
BASIS_UNKNOWN = "unknown"


@dataclass(frozen=True)
class Finding:
    """One thing the audit wants a person to see."""

    severity: str
    title: str
    detail: str


@dataclass(frozen=True)
class SecretAge:
    """Audit row for one organization secret."""

    name: str
    updated_at: str
    age_days: int
    limit_days: int
    obsolete_reason: str
    credential: bool = True

    @property
    def overdue(self) -> bool:
        return self.credential and self.age_days > self.limit_days

    @property
    def status(self) -> str:
        if self.obsolete_reason:
            return "obsolete: delete"
        if not self.credential:
            return "not a credential"
        if self.overdue:
            return "rotate"
        return "ok"


@dataclass(frozen=True)
class KeyAge:
    """How old the stored App private key is, and what that age counts from."""

    basis: str
    since: str
    age_days: int | None


@dataclass(frozen=True)
class Audit:
    """Everything the workflow collected for one run."""

    owner: str
    expected_repos: frozenset[str]
    mint_outcome: str
    repos_outcome: str
    installation_repos: frozenset[str] | None
    secrets_outcome: str
    secrets: tuple[dict, ...]
    key_secret: str
    key_secret_repos: frozenset[str] | None
    key_max_age_days: int
    max_age_days: int
    obsolete: dict[str, str]
    now: datetime
    not_credentials: frozenset[str] = frozenset()
    repos_file: str = REPOS_FILE
    app_id_variable: str = "TF_AUTOMATION_APP_ID"
    variable_outcome: str = "skipped"
    variable_visibility: str = ""
    variable_repos: frozenset[str] | None = None
    key_fingerprint: str = ""
    recorded_fingerprint: str = ""
    recorded_rotated_at: str = ""
    health_repo: str = "github_actions"


def parse_json_stream(text: str) -> list[dict]:
    """Decode concatenated or newline-delimited JSON objects (``gh api --paginate --jq``)."""
    decoder = json.JSONDecoder()
    items: list[dict] = []
    index = 0
    while True:
        while index < len(text) and text[index].isspace():
            index += 1
        if index >= len(text):
            return items
        value, index = decoder.raw_decode(text, index)
        if isinstance(value, list):
            items.extend(value)
        else:
            items.append(value)


def parse_names(text: str) -> frozenset[str]:
    """Repository names, one per line, optionally ``owner/name``; compared case-insensitively.

    ``#`` starts a comment, so the checked-in list can explain itself.
    """
    names = set()
    for line in text.splitlines():
        name = line.split("#", 1)[0].strip()
        if name:
            names.add(name.rsplit("/", 1)[-1].lower())
    return frozenset(names)


def parse_timestamp(value: str) -> datetime:
    """Parse an ISO 8601 date or timestamp; a bare date or a naive time is UTC."""
    parsed = datetime.fromisoformat(value.strip().replace("Z", "+00:00"))
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=timezone.utc)
    return parsed


def parse_obsolete(entries: list[str]) -> dict[str, str]:
    obsolete = {}
    for entry in entries:
        name, separator, reason = entry.partition("=")
        if not separator or not name.strip():
            raise ValueError(f"--obsolete expects NAME=reason, got {entry!r}")
        obsolete[name.strip()] = reason.strip()
    return obsolete


def normalize_fingerprint(value: str) -> str:
    """``SHA256:abc=`` and ``abc`` name the same key; GitHub shows the prefixed, padded form."""
    value = value.strip()
    if value.upper().startswith("SHA256:"):
        value = value[len("SHA256:"):]
    return value.rstrip("=")


def code_list(names: frozenset[str] | set[str]) -> str:
    return ", ".join(f"`{name}`" for name in sorted(names))


def secret_entry(audit: Audit) -> dict | None:
    return next((item for item in audit.secrets if item.get("name") == audit.key_secret), None)


def record_commands(audit: Audit, fingerprint: str = "", rotated_at: str = "") -> str:
    """The one-time commands that create the key rotation record."""
    fingerprint = fingerprint or audit.key_fingerprint or "SHA256:<fingerprint of the stored key>"
    rotated_at = rotated_at or "<YYYY-MM-DD the key was generated>"
    scope = f"--org {audit.owner} --visibility selected --repos {audit.health_repo}"
    return (f"`gh variable set {FINGERPRINT_VARIABLE} {scope} --body '{fingerprint}'` and"
            f" `gh variable set {ROTATED_AT_VARIABLE} {scope} --body '{rotated_at}'`")


def age_in_days(audit: Audit, since: str) -> int:
    return (audit.now - parse_timestamp(since)).days


def key_age(audit: Audit) -> tuple[KeyAge, list[Finding]]:
    """Date the stored key from its rotation record, or from the secret's last update.

    The rotation record (two organization variables the rotation script
    writes) only counts when its fingerprint matches the key actually stored in
    the secret. The fallback, the secret's ``updated_at``, also moves whenever
    the secret's repository access changes, so it can make the key look newer
    than it is.
    """
    entry = secret_entry(audit) if audit.secrets_outcome == "success" else None
    updated = (entry or {}).get("updated_at") or (entry or {}).get("created_at") or ""
    fallback = (KeyAge(BASIS_UPDATED, updated, age_in_days(audit, updated)) if updated
                else KeyAge(BASIS_UNKNOWN, "", None))
    fallback_text = ("Its age counts from the secret's last update instead, which also changes whenever the"
                     " secret's repository access changes.")
    recorded_fp = audit.recorded_fingerprint.strip()
    recorded_at = audit.recorded_rotated_at.strip()

    if not recorded_fp and not recorded_at:
        return fallback, [Finding(
            NOTE, "The App private key has no rotation record",
            f"`{FINGERPRINT_VARIABLE}` and `{ROTATED_AT_VARIABLE}` are not set (or not visible to"
            f" `{audit.health_repo}`). {fallback_text} Record the stored key once with"
            f" {record_commands(audit)}; {ROTATE_SCRIPT} keeps both current from then on.",
        )]
    if not recorded_fp or not recorded_at:
        missing = FINGERPRINT_VARIABLE if not recorded_fp else ROTATED_AT_VARIABLE
        return fallback, [Finding(
            WARNING, "The App private key rotation record is incomplete",
            f"`{missing}` is not set. {fallback_text} Set both: {record_commands(audit)}.",
        )]
    try:
        rotated = parse_timestamp(recorded_at)
    except ValueError:
        return fallback, [Finding(
            WARNING, f"`{ROTATED_AT_VARIABLE}` is not a date",
            f"It holds `{recorded_at}`; expected `YYYY-MM-DD` or an ISO 8601 UTC timestamp. {fallback_text}",
        )]
    if rotated > audit.now:
        return fallback, [Finding(
            WARNING, f"`{ROTATED_AT_VARIABLE}` is in the future",
            f"It holds `{recorded_at}`. {fallback_text}",
        )]
    if not audit.key_fingerprint.strip():
        return fallback, [Finding(
            NOTE, "The App private key rotation record was not verified",
            f"The run could not fingerprint the key stored in `{audit.key_secret}`, so it cannot tell whether"
            f" the record describes that key. {fallback_text}",
        )]
    if normalize_fingerprint(recorded_fp) != normalize_fingerprint(audit.key_fingerprint):
        return fallback, [Finding(
            WARNING, "The App private key rotation record describes a different key",
            f"`{FINGERPRINT_VARIABLE}` names `{recorded_fp}`, but `{audit.key_secret}` holds"
            f" `{audit.key_fingerprint}`: the secret was replaced without {ROTATE_SCRIPT}. {fallback_text}"
            f" Record the stored key with {record_commands(audit)}.",
        )]
    return KeyAge(BASIS_RECORD, recorded_at, (audit.now - rotated).days), []


def key_rotation_findings(audit: Audit, key: KeyAge) -> list[Finding]:
    if key.age_days is None or key.age_days <= audit.key_max_age_days:
        return []
    verb = "Rotated" if key.basis == BASIS_RECORD else "Last updated"
    return [Finding(
        WARNING, f"App private key `{audit.key_secret}` is due for rotation",
        f"{verb} {key.since[:10]} ({key.age_days} days ago); the limit is {audit.key_max_age_days} days."
        f" See {RUNBOOK}.",
    )]


def secret_ages(audit: Audit, key: KeyAge) -> list[SecretAge]:
    rows = []
    for secret in sorted(audit.secrets, key=lambda item: item["name"]):
        name = secret["name"]
        updated_at = secret.get("updated_at") or secret.get("created_at") or ""
        age_days = age_in_days(audit, updated_at) if updated_at else 0
        limit = audit.max_age_days
        if name == audit.key_secret:
            limit = audit.key_max_age_days
            if key.age_days is not None:
                age_days = key.age_days
        rows.append(SecretAge(name, updated_at, age_days, limit, audit.obsolete.get(name, ""),
                              credential=name not in audit.not_credentials))
    return rows


def drift_detail(missing: list[str], unlisted: list[str], missing_text: str, unlisted_text: str) -> str:
    parts = []
    if missing:
        parts.append(f"{missing_text}: " + ", ".join(f"`{name}`" for name in missing))
    if unlisted:
        parts.append(f"{unlisted_text}: " + ", ".join(f"`{name}`" for name in unlisted))
    return "; ".join(parts) + "."


def installation_findings(audit: Audit) -> list[Finding]:
    if audit.repos_outcome != "success" or audit.installation_repos is None:
        return [Finding(ERROR, "Could not list the installation's repositories",
                        "The App minted a token but `GET /installation/repositories` failed; see the run log.")]
    missing = sorted(audit.expected_repos - audit.installation_repos)
    unlisted = sorted(audit.installation_repos - audit.expected_repos)
    if not missing and not unlisted:
        return []
    return [Finding(
        ERROR, f"The App installation's repositories differ from {audit.repos_file}",
        drift_detail(missing, unlisted, "Listed but not reachable (re-add them under the installation's"
                     " repository access)", "Reachable but not listed (remove them from the installation)")
        + f" If the change was intended, update {audit.repos_file}, the installation, the"
        f" `{audit.key_secret}` secret, and the `{audit.app_id_variable}` variable together.",
    )]


def shared_access_findings(audit: Audit, name: str, kind: str, visibility: str,
                           repos: frozenset[str] | None, fix: str) -> list[Finding]:
    """Compare one organization secret's or variable's repository access with the list."""
    if visibility != "selected":
        return [Finding(
            ERROR, f"`{name}` is visible to {visibility or 'unknown'} repositories",
            f"Every repository in that scope can read the {kind}. Set its visibility to selected, limited to"
            f" the repositories in {audit.repos_file}: {fix}.",
        )]
    if repos is None:
        return [Finding(
            WARNING, f"Could not read which repositories can read `{name}`",
            f"Listing its selected repositories failed, so its access was not compared with {audit.repos_file};"
            " see the run log.",
        )]
    missing = sorted(audit.expected_repos - repos)
    unlisted = sorted(repos - audit.expected_repos)
    if not missing and not unlisted:
        return []
    return [Finding(
        ERROR, f"`{name}` repository access differs from {audit.repos_file}",
        drift_detail(missing, unlisted, f"Listed but cannot read the {kind} (their workflows cannot mint App"
                     " tokens)", f"Can read the {kind} but are not listed")
        + f" Share it with exactly the listed repositories: {fix}.",
    )]


def key_access_findings(audit: Audit) -> list[Finding]:
    """The key must be an organization secret shared with exactly the listed repositories."""
    if audit.secrets_outcome != "success":
        return []  # secret_findings already explains why the audit did not run
    entry = secret_entry(audit)
    if entry is None:
        return [Finding(
            WARNING, f"`{audit.key_secret}` is not an organization secret",
            "The App minted a token, so the key comes from somewhere else (a repository secret?)."
            f" Store it as the organization secret {audit.key_secret} with selected repositories"
            f" ({ROTATE_SCRIPT}).",
        )]
    fix = (f"`{ROTATE_SCRIPT} --sync-repos` at the next rotation, or the secret's repository access in the"
           " organization's Actions secrets settings")
    return shared_access_findings(audit, audit.key_secret, "App private key", entry.get("visibility", ""),
                                  audit.key_secret_repos, fix)


def variable_findings(audit: Audit) -> list[Finding]:
    """The App ID variable must be shared with exactly the listed repositories."""
    name = audit.app_id_variable
    if audit.variable_outcome == "missing-permission":
        return [Finding(
            WARNING, f"`{name}` repository access not checked: the App lacks organization \"Variables: read\"",
            "Grant the threatflux-automation App the organization permission Variables: Read-only and approve"
            f" the updated permissions for the {audit.owner} installation. Until then the weekly run cannot"
            f" compare the variable with {audit.repos_file}; `{ROTATE_SCRIPT} --check-repos`, run by an"
            f" organization owner, does. See {RUNBOOK}.",
        )]
    if audit.variable_outcome == "not-found":
        return [Finding(
            WARNING, f"`{name}` is not an organization variable",
            "The App minted a token, so the App ID comes from somewhere else (a repository variable?)."
            f" Store it as the organization variable {name} with selected repositories.",
        )]
    if audit.variable_outcome != "success":
        detail = ("Reading the organization variable failed unexpectedly; see the run log."
                  if audit.variable_outcome == "failure" else "The step that reads it did not run; see the run log.")
        return [Finding(WARNING, f"Could not read the `{name}` variable", detail)]
    listed = ",".join(sorted(audit.expected_repos))
    fix = (f"`gh variable set {name} --org {audit.owner} --visibility selected --repos {listed}"
           " --body <App ID>`")
    return shared_access_findings(audit, name, "App ID", audit.variable_visibility, audit.variable_repos, fix)


def secret_findings(audit: Audit, rows: list[SecretAge]) -> list[Finding]:
    if audit.secrets_outcome == "missing-permission":
        return [Finding(
            WARNING, "Organization secret audit skipped: the App lacks organization \"Secrets: read\"",
            "Grant the threatflux-automation App the organization permission Secrets: Read-only and approve"
            f" the updated permissions for the {audit.owner} installation. Until then secret ages, obsolete"
            f" secrets, and the key secret's repository access are not checked. See {RUNBOOK}.",
        )]
    if audit.secrets_outcome != "success":
        return [Finding(WARNING, "Organization secret audit failed",
                        "Listing the organization's Actions secrets failed unexpectedly; see the run log.")]

    findings = []
    for row in rows:
        if row.obsolete_reason:
            findings.append(Finding(WARNING, f"Obsolete secret `{row.name}` still exists", row.obsolete_reason))
        elif row.overdue and row.name != audit.key_secret:
            findings.append(Finding(
                WARNING, f"Secret `{row.name}` is due for rotation",
                f"Last updated {row.updated_at[:10]} ({row.age_days} days ago); the limit is"
                f" {row.limit_days} days. See {RUNBOOK}.",
            ))
    return findings


def evaluate(audit: Audit) -> tuple[str, list[Finding], list[SecretAge]]:
    """Return the status, the findings (most severe first), and the secret audit rows."""
    if audit.mint_outcome != "success":
        findings = [Finding(
            ERROR, "The App could not mint an installation token",
            "Check the TF_AUTOMATION_APP_ID organization variable and the TF_AUTOMATION_APP_PRIVATE_KEY"
            " organization secret (their selected repositories must include this one). After a key rotation"
            " this means the secret holds a key that was deleted or never belonged to the App; rerun"
            f" {ROTATE_SCRIPT} with a fresh key. See {RUNBOOK}.",
        )]
        return "broken", findings, []

    key, key_findings = key_age(audit)
    rows = secret_ages(audit, key) if audit.secrets_outcome == "success" else []
    findings = (installation_findings(audit) + key_access_findings(audit) + variable_findings(audit)
                + secret_findings(audit, rows) + key_rotation_findings(audit, key) + key_findings)
    findings.sort(key=lambda finding: SEVERITY_ORDER[finding.severity])
    if any(finding.severity == ERROR for finding in findings):
        status = "broken"
    elif any(finding.severity == WARNING for finding in findings):
        status = "attention"
    else:
        status = "healthy"
    return status, findings, rows


def access_result(audit: Audit, repos: frozenset[str] | None, unavailable: str) -> str:
    if repos is None:
        return unavailable
    listed = len(repos & audit.expected_repos)
    unlisted = len(repos - audit.expected_repos)
    text = f"{listed} of {len(audit.expected_repos)} listed"
    if unlisted:
        text += f", {unlisted} not listed"
    return text + ("" if repos == audit.expected_repos else ", **drift**")


def check_rows(audit: Audit, key: KeyAge) -> list[str]:
    rows = ["| Installation token | " + ("minted" if audit.mint_outcome == "success" else "**failed**") + " |"]
    if audit.mint_outcome != "success":
        return rows
    rows.append("| Installation repositories | "
                + access_result(audit, audit.installation_repos, "**not listed**") + " |")

    entry = secret_entry(audit) if audit.secrets_outcome == "success" else None
    if audit.secrets_outcome != "success":
        key_access = "skipped: organization secret audit did not run"
    elif entry is None:
        key_access = "**not an organization secret**"
    elif entry.get("visibility") != "selected":
        key_access = f"**visible to {entry.get('visibility') or 'unknown'} repositories**"
    else:
        key_access = access_result(audit, audit.key_secret_repos, "**not listed**")
    rows.append(f"| `{audit.key_secret}` repositories | {key_access} |")

    variable_access = {
        "missing-permission": "skipped: App lacks organization Variables: read",
        "not-found": "**not an organization variable**",
        "failure": "**failed**",
        "skipped": "skipped",
    }.get(audit.variable_outcome)
    if variable_access is None:
        variable_access = (access_result(audit, audit.variable_repos, "**not listed**")
                           if audit.variable_visibility == "selected"
                           else f"**visible to {audit.variable_visibility or 'unknown'} repositories**")
    rows.append(f"| `{audit.app_id_variable}` repositories | {variable_access} |")

    if key.age_days is None:
        age = "unknown"
    else:
        basis = "from its rotation record" if key.basis == BASIS_RECORD else "from the secret's last update"
        age = f"{key.age_days} days, {basis}"
    rows.append(f"| App private key age | {age} (limit {audit.key_max_age_days}) |")
    return rows


def render_report(audit: Audit, result: tuple[str, list[Finding], list[SecretAge]],
                  run_url: str = "", repo_url: str = "") -> str:
    """Render the Markdown report for the job summary and the tracking issue."""
    status, findings, rows = result
    key, _ = key_age(audit)
    lines = [REPORT_MARKER, f"## Automation App health: {status}", ""]
    checked = audit.now.strftime("%Y-%m-%d %H:%M UTC")
    lines.append(f"Checked {checked}" + (f" by [this run]({run_url})." if run_url else "."))
    lines += ["", "| Check | Result |", "| --- | --- |"]
    lines += check_rows(audit, key)
    secret_result = {
        "success": f"{len(rows)} secrets audited",
        "missing-permission": "skipped: App lacks organization Secrets: read",
        "failure": "**failed**",
    }.get(audit.secrets_outcome, "skipped")
    lines.append(f"| Organization secret audit | {secret_result} |")

    problems = [finding for finding in findings if finding.severity != NOTE]
    notes = [finding for finding in findings if finding.severity == NOTE]
    lines += ["", "### Findings", ""]
    if problems:
        lines += [f"- **{finding.severity}**: {finding.title}. {finding.detail}" for finding in problems]
    else:
        lines.append("None.")
    if notes:
        lines += ["", "### Notes", ""]
        lines += [f"- {finding.title}. {finding.detail}" for finding in notes]

    if audit.mint_outcome == "success":
        lines += ["", "### App private key", "", "| | |", "| --- | --- |"]
        lines.append(f"| Stored key | `{audit.key_fingerprint}` |" if audit.key_fingerprint
                     else "| Stored key | fingerprint unavailable |")
        if audit.recorded_fingerprint or audit.recorded_rotated_at:
            lines.append(f"| Rotation record | `{audit.recorded_fingerprint or 'unset'}`, rotated"
                         f" {audit.recorded_rotated_at or 'unset'} |")
        else:
            lines.append("| Rotation record | none |")
        lines += ["", "The App's settings list the fingerprint of each of its private keys. Only the stored key is"
                      " in use, so any other key can be deleted."]

    lines += ["", "### Expected repositories", ""]
    lines.append(f"{len(audit.expected_repos)} repositories from `{audit.repos_file}`: "
                 + (code_list(audit.expected_repos) or "none") + ".")
    if audit.installation_repos is not None and audit.installation_repos != audit.expected_repos:
        lines += ["", "Installation repositories: " + (code_list(audit.installation_repos) or "none") + "."]

    if rows:
        lines += ["", "### Organization secrets", "",
                  "| Secret | Last updated | Age (days) | Limit (days) | Status |",
                  "| --- | --- | ---: | ---: | --- |"]
        for row in rows:
            lines.append(f"| `{row.name}` | {row.updated_at[:10]} | {row.age_days} | {row.limit_days}"
                         f" | {row.status} |")
        if key.basis == BASIS_RECORD:
            lines += ["", f"The age of `{audit.key_secret}` counts from its rotation record"
                          f" ({key.since[:10]}), not from its last update."]
        lines += ["", "Only names and dates are read; GitHub never returns secret values."]

    runbook = f"[{RUNBOOK}]({repo_url}/blob/main/{RUNBOOK})" if repo_url else RUNBOOK
    lines += ["", f"Runbook: {runbook}."]
    return "\n".join(lines) + "\n"


def read_optional(path: str | None) -> str | None:
    if not path:
        return None
    file = Path(path)
    return file.read_text(encoding="utf-8") if file.is_file() else None


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    parser.add_argument("--owner", required=True)
    parser.add_argument("--expected-repos-file", required=True,
                        help=f"the checked-in repository list, normally {REPOS_FILE}")
    parser.add_argument("--mint-outcome", required=True)
    parser.add_argument("--repos-outcome", default="skipped")
    parser.add_argument("--repos-file")
    parser.add_argument("--secrets-outcome", default="skipped", choices=SECRETS_OUTCOMES)
    parser.add_argument("--secrets-file")
    parser.add_argument("--key-secret", required=True, help="name of the org secret holding the App private key")
    parser.add_argument("--key-secret-repos-file")
    parser.add_argument("--app-id-variable", default="TF_AUTOMATION_APP_ID")
    parser.add_argument("--variable-outcome", default="skipped", choices=VARIABLE_OUTCOMES)
    parser.add_argument("--variable-visibility", default="")
    parser.add_argument("--variable-repos-file")
    parser.add_argument("--key-fingerprint", default="", help="SHA256 fingerprint of the key stored in the secret")
    parser.add_argument("--recorded-fingerprint", default="", help=f"value of {FINGERPRINT_VARIABLE}")
    parser.add_argument("--recorded-rotated-at", default="", help=f"value of {ROTATED_AT_VARIABLE}")
    parser.add_argument("--health-repo", default="github_actions",
                        help="repository the rotation record variables must be visible to")
    parser.add_argument("--key-max-age-days", type=int, default=DEFAULT_KEY_MAX_AGE_DAYS)
    parser.add_argument("--max-age-days", type=int, default=DEFAULT_MAX_AGE_DAYS)
    parser.add_argument("--obsolete", action="append", default=[], metavar="NAME=REASON")
    parser.add_argument("--not-credential", action="append", default=[], metavar="NAME",
                        help="a secret that only holds a name (e.g. a username), so it never needs rotation")
    parser.add_argument("--now", help="ISO 8601 timestamp; defaults to the current time")
    parser.add_argument("--run-url", default="")
    parser.add_argument("--repo-url", default="", help="links the runbook, e.g. https://github.com/OWNER/REPO")
    parser.add_argument("--report", required=True, help="where to write the Markdown report")
    parser.add_argument("--github-output", default=os.environ.get("GITHUB_OUTPUT", ""))
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    expected_text = read_optional(args.expected_repos_file)
    if expected_text is None:
        raise SystemExit(f"error: {args.expected_repos_file} does not exist")
    expected = parse_names(expected_text)
    if not expected:
        raise SystemExit(f"error: {args.expected_repos_file} lists no repository")
    repos_text = read_optional(args.repos_file)
    secrets_text = read_optional(args.secrets_file)
    key_repos_text = read_optional(args.key_secret_repos_file)
    variable_repos_text = read_optional(args.variable_repos_file)
    secrets_outcome = args.secrets_outcome
    if secrets_outcome == "success" and secrets_text is None:
        secrets_outcome = "failure"

    audit = Audit(
        owner=args.owner,
        expected_repos=expected,
        mint_outcome=args.mint_outcome,
        repos_outcome=args.repos_outcome if repos_text is not None else "failure",
        installation_repos=parse_names(repos_text) if repos_text is not None else None,
        secrets_outcome=secrets_outcome,
        secrets=tuple(parse_json_stream(secrets_text)) if secrets_outcome == "success" and secrets_text else (),
        key_secret=args.key_secret,
        key_secret_repos=parse_names(key_repos_text) if key_repos_text is not None else None,
        key_max_age_days=args.key_max_age_days,
        max_age_days=args.max_age_days,
        obsolete=parse_obsolete(args.obsolete),
        not_credentials=frozenset(args.not_credential),
        now=parse_timestamp(args.now) if args.now else datetime.now(timezone.utc),
        repos_file=args.expected_repos_file,
        app_id_variable=args.app_id_variable,
        variable_outcome=args.variable_outcome,
        variable_visibility=args.variable_visibility.strip(),
        variable_repos=parse_names(variable_repos_text) if variable_repos_text is not None else None,
        key_fingerprint=args.key_fingerprint.strip(),
        recorded_fingerprint=args.recorded_fingerprint.strip(),
        recorded_rotated_at=args.recorded_rotated_at.strip(),
        health_repo=args.health_repo,
    )
    result = evaluate(audit)
    status, findings, _ = result
    report = render_report(audit, result, args.run_url, args.repo_url)
    Path(args.report).write_text(report, encoding="utf-8")

    errors = sum(1 for finding in findings if finding.severity == ERROR)
    problems = sum(1 for finding in findings if finding.severity != NOTE)
    notes = len(findings) - problems
    print(f"status={status} findings={problems} errors={errors} notes={notes}")
    for finding in findings:
        print(f"[{finding.severity}] {finding.title}")
    if args.github_output:
        with open(args.github_output, "a", encoding="utf-8") as output:
            output.write(f"status={status}\nfindings={problems}\nerrors={errors}\nnotes={notes}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
