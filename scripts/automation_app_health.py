#!/usr/bin/env python3
"""Turn the automation App health facts into findings, a report, and a status.

automation-app-health.yml gathers the raw facts with the App's own tokens: the
repositories the installation reaches and, when the App holds the organization
"Secrets: read" permission, the metadata (names and dates, never values) of
the organization's Actions secrets. This script judges them:

- ``broken``: the App cannot mint a token, or its installation does not reach
  exactly the expected repositories. The workflow fails.
- ``attention``: the App works, but a person has to act: a secret is due for
  rotation, an obsolete secret still exists, or the secret audit could not run.
- ``healthy``: nothing to report, so the tracking issue is closed.
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

DEFAULT_KEY_MAX_AGE_DAYS = 90
DEFAULT_MAX_AGE_DAYS = 180
REPORT_MARKER = "<!-- automation-app-health -->"
RUNBOOK = "docs/SECRETS-ROTATION.md"

SECRETS_OUTCOMES = ("success", "missing-permission", "failure", "skipped")


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
    """Repository names, one per line, optionally ``owner/name``; compared case-insensitively."""
    names = set()
    for line in text.splitlines():
        name = line.strip()
        if name:
            names.add(name.rsplit("/", 1)[-1].lower())
    return frozenset(names)


def parse_timestamp(value: str) -> datetime:
    parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
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


def secret_ages(audit: Audit) -> list[SecretAge]:
    rows = []
    for secret in sorted(audit.secrets, key=lambda item: item["name"]):
        name = secret["name"]
        updated_at = secret.get("updated_at") or secret.get("created_at") or ""
        age_days = (audit.now - parse_timestamp(updated_at)).days if updated_at else 0
        limit = audit.key_max_age_days if name == audit.key_secret else audit.max_age_days
        rows.append(SecretAge(name, updated_at, age_days, limit, audit.obsolete.get(name, ""),
                              credential=name not in audit.not_credentials))
    return rows


def repository_findings(audit: Audit) -> list[Finding]:
    if audit.repos_outcome != "success" or audit.installation_repos is None:
        return [Finding(ERROR, "Could not list the installation's repositories",
                        "The App minted a token but `GET /installation/repositories` failed; see the run log.")]
    findings = []
    missing = sorted(audit.expected_repos - audit.installation_repos)
    unexpected = sorted(audit.installation_repos - audit.expected_repos)
    if missing:
        findings.append(Finding(
            ERROR, "Installation is missing expected repositories",
            "The App can no longer reach: " + ", ".join(f"`{name}`" for name in missing)
            + ". Re-add them under the installation's repository access.",
        ))
    if unexpected:
        findings.append(Finding(
            ERROR, "Installation reaches unexpected repositories",
            "The App can also reach: " + ", ".join(f"`{name}`" for name in unexpected)
            + ". Remove them, or add them to EXPECTED_REPOSITORIES in automation-app-health.yml"
            " (and to the private key secret's selected repositories) if that was intended.",
        ))
    return findings


def secret_findings(audit: Audit, rows: list[SecretAge]) -> list[Finding]:
    if audit.secrets_outcome == "missing-permission":
        return [Finding(
            WARNING, "Organization secret audit skipped: the App lacks organization \"Secrets: read\"",
            "Grant the threatflux-automation App the organization permission Secrets: Read-only and approve"
            f" the updated permissions for the {audit.owner} installation. Until then secret ages and"
            f" obsolete secrets are not checked. See {RUNBOOK}.",
        )]
    if audit.secrets_outcome != "success":
        return [Finding(WARNING, "Organization secret audit failed",
                        "Listing the organization's Actions secrets failed unexpectedly; see the run log.")]

    findings = []
    for row in rows:
        if row.obsolete_reason:
            findings.append(Finding(WARNING, f"Obsolete secret `{row.name}` still exists", row.obsolete_reason))
        elif row.overdue:
            kind = "App private key" if row.name == audit.key_secret else "Secret"
            findings.append(Finding(
                WARNING, f"{kind} `{row.name}` is due for rotation",
                f"Last updated {row.updated_at[:10]} ({row.age_days} days ago); the limit is"
                f" {row.limit_days} days. See {RUNBOOK}.",
            ))
    return findings + key_access_findings(audit)


def key_access_findings(audit: Audit) -> list[Finding]:
    """Check the App key is an org secret shared with exactly the installation's repositories."""
    entry = next((item for item in audit.secrets if item.get("name") == audit.key_secret), None)
    if entry is None:
        return [Finding(
            WARNING, f"`{audit.key_secret}` is not an organization secret",
            "The App minted a token, so the key comes from somewhere else (a repository secret?)."
            f" Store it as the organization secret {audit.key_secret} with selected repositories.",
        )]
    visibility = entry.get("visibility", "")
    if visibility != "selected":
        return [Finding(
            WARNING, f"`{audit.key_secret}` is visible to {visibility or 'unknown'} repositories",
            "Every repository in that scope can read the App private key. Set its visibility to selected,"
            " limited to the installation's repositories (scripts/rotate-automation-app-key.sh --repos).",
        )]
    if audit.key_secret_repos is None:
        return [Finding(
            WARNING, f"Could not read which repositories can read `{audit.key_secret}`",
            "Listing the secret's selected repositories failed, so its access was not compared with the"
            " installation; see the run log.",
        )]
    if audit.installation_repos is None or audit.key_secret_repos == audit.installation_repos:
        return []
    only_secret = sorted(audit.key_secret_repos - audit.installation_repos)
    only_install = sorted(audit.installation_repos - audit.key_secret_repos)
    parts = []
    if only_install:
        parts.append("installed but cannot read the key: " + ", ".join(f"`{n}`" for n in only_install))
    if only_secret:
        parts.append("can read the key but are not installed: " + ", ".join(f"`{n}`" for n in only_secret))
    return [Finding(
        WARNING, f"`{audit.key_secret}` repository access differs from the installation",
        "Repositories " + "; ".join(parts) + ".",
    )]


def evaluate(audit: Audit) -> tuple[str, list[Finding], list[SecretAge]]:
    """Return the status, the findings, and the secret audit rows."""
    if audit.mint_outcome != "success":
        findings = [Finding(
            ERROR, "The App could not mint an installation token",
            "Check the TF_AUTOMATION_APP_ID organization variable and the TF_AUTOMATION_APP_PRIVATE_KEY"
            " organization secret (its selected repositories must include this one). After a key rotation"
            " this means the secret holds a key that was deleted or never belonged to the App; rerun"
            f" scripts/rotate-automation-app-key.sh with a fresh key. See {RUNBOOK}.",
        )]
        return "broken", findings, []

    rows = secret_ages(audit) if audit.secrets_outcome == "success" else []
    findings = repository_findings(audit) + secret_findings(audit, rows)
    if any(finding.severity == ERROR for finding in findings):
        status = "broken"
    elif findings:
        status = "attention"
    else:
        status = "healthy"
    return status, findings, rows


def render_report(audit: Audit, result: tuple[str, list[Finding], list[SecretAge]],
                  run_url: str = "", repo_url: str = "") -> str:
    """Render the Markdown report for the job summary and the tracking issue."""
    status, findings, rows = result
    lines = [REPORT_MARKER, f"## Automation App health: {status}", ""]
    checked = audit.now.strftime("%Y-%m-%d %H:%M UTC")
    lines.append(f"Checked {checked}" + (f" by [this run]({run_url})." if run_url else "."))
    lines += ["", "| Check | Result |", "| --- | --- |"]
    lines.append("| Installation token | " + ("minted" if audit.mint_outcome == "success" else "**failed**") + " |")
    if audit.mint_outcome == "success" and audit.installation_repos is not None:
        reached = len(audit.installation_repos & audit.expected_repos)
        matches = audit.installation_repos == audit.expected_repos
        lines.append(f"| Installation repositories | {reached} of {len(audit.expected_repos)} expected"
                     + ("" if matches else ", **mismatch**") + " |")
    elif audit.mint_outcome == "success":
        lines.append("| Installation repositories | **not listed** |")
    secret_result = {
        "success": f"{len(rows)} secrets audited",
        "missing-permission": "skipped: App lacks organization Secrets: read",
        "failure": "**failed**",
    }.get(audit.secrets_outcome, "skipped")
    lines.append(f"| Organization secret audit | {secret_result} |")

    lines += ["", "### Findings", ""]
    if findings:
        for finding in findings:
            lines.append(f"- **{finding.severity}**: {finding.title}. {finding.detail}")
    else:
        lines.append("None.")

    if audit.installation_repos is not None:
        lines += ["", "### Installation repositories", ""]
        lines.append(", ".join(f"`{name}`" for name in sorted(audit.installation_repos)) or "None.")

    if rows:
        lines += ["", "### Organization secrets", "",
                  "| Secret | Last updated | Age (days) | Limit (days) | Status |",
                  "| --- | --- | ---: | ---: | --- |"]
        for row in rows:
            lines.append(f"| `{row.name}` | {row.updated_at[:10]} | {row.age_days} | {row.limit_days}"
                         f" | {row.status} |")
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
    parser.add_argument("--expected-repos", required=True, help="comma-separated repository names")
    parser.add_argument("--mint-outcome", required=True)
    parser.add_argument("--repos-outcome", default="skipped")
    parser.add_argument("--repos-file")
    parser.add_argument("--secrets-outcome", default="skipped", choices=SECRETS_OUTCOMES)
    parser.add_argument("--secrets-file")
    parser.add_argument("--key-secret", required=True, help="name of the org secret holding the App private key")
    parser.add_argument("--key-secret-repos-file")
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
    repos_text = read_optional(args.repos_file)
    secrets_text = read_optional(args.secrets_file)
    key_repos_text = read_optional(args.key_secret_repos_file)
    secrets_outcome = args.secrets_outcome
    if secrets_outcome == "success" and secrets_text is None:
        secrets_outcome = "failure"

    audit = Audit(
        owner=args.owner,
        expected_repos=parse_names(args.expected_repos.replace(",", "\n")),
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
    )
    result = evaluate(audit)
    status, findings, _ = result
    report = render_report(audit, result, args.run_url, args.repo_url)
    Path(args.report).write_text(report, encoding="utf-8")

    errors = sum(1 for finding in findings if finding.severity == ERROR)
    print(f"status={status} findings={len(findings)} errors={errors}")
    for finding in findings:
        print(f"[{finding.severity}] {finding.title}")
    if args.github_output:
        with open(args.github_output, "a", encoding="utf-8") as output:
            output.write(f"status={status}\nfindings={len(findings)}\nerrors={errors}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
