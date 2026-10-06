"""Judge the automation App health facts the way automation-app-health.yml does."""

import contextlib
import io
import json
import tempfile
import unittest
from datetime import datetime, timezone
from pathlib import Path

from automation_app_health import Audit, evaluate, main, parse_json_stream, parse_names, render_report

NOW = datetime(2026, 10, 6, 12, 0, tzinfo=timezone.utc)
EXPECTED = frozenset({"github_actions", "ollama_rust_sdk", "rust-cicd-template"})


def inventory_entry(name: str, updated: str) -> dict:
    return {"name": name, "created_at": updated, "updated_at": updated, "visibility": "all"}


def audit(**overrides) -> Audit:
    values = {
        "owner": "ThreatFlux",
        "expected_repos": EXPECTED,
        "mint_outcome": "success",
        "repos_outcome": "success",
        "installation_repos": EXPECTED,
        "secrets_outcome": "success",
        "secrets": (
            inventory_entry("TF_AUTOMATION_APP_PRIVATE_KEY", "2026-09-01T00:00:00Z"),
            inventory_entry("CODECOV_TOKEN", "2026-06-01T00:00:00Z"),
        ),
        "key_secret": "TF_AUTOMATION_APP_PRIVATE_KEY",  # nosec B105: a secret name, not a value
        "key_secret_repos": EXPECTED,
        "key_max_age_days": 90,
        "max_age_days": 180,
        "obsolete": {"GIT_TOKEN": "Dead classic PAT."},  # nosec B105: a finding text, not a value
        "now": NOW,
    }
    values.update(overrides)
    return Audit(**values)


class EvaluateTests(unittest.TestCase):
    def test_fresh_secrets_and_exact_repositories_are_healthy(self) -> None:
        status, findings, rows = evaluate(audit())
        self.assertEqual(status, "healthy")
        self.assertEqual(findings, [])
        self.assertEqual([row.status for row in rows], ["ok", "ok"])

    def test_mint_failure_is_broken_and_checks_nothing_else(self) -> None:
        status, findings, rows = evaluate(audit(mint_outcome="failure", installation_repos=None))
        self.assertEqual(status, "broken")
        self.assertEqual(len(findings), 1)
        self.assertIn("could not mint", findings[0].title)
        self.assertEqual(rows, [])

    def test_missing_or_extra_repositories_are_broken(self) -> None:
        for repos, word in (
            (EXPECTED - {"ollama_rust_sdk"}, "missing"),
            (EXPECTED | {"lifeflux"}, "unexpected"),
        ):
            with self.subTest(word=word):
                status, findings, _ = evaluate(audit(installation_repos=frozenset(repos), key_secret_repos=None))
                self.assertEqual(status, "broken")
                self.assertTrue(any(word in finding.title for finding in findings))

    def test_unlisted_repositories_are_broken(self) -> None:
        status, findings, _ = evaluate(audit(repos_outcome="failure", installation_repos=None))
        self.assertEqual(status, "broken")
        self.assertIn("Could not list", findings[0].title)

    def test_key_older_than_ninety_days_needs_rotation(self) -> None:
        status, findings, rows = evaluate(audit(secrets=(
            inventory_entry("TF_AUTOMATION_APP_PRIVATE_KEY", "2026-07-01T00:00:00Z"),
        )))
        self.assertEqual(status, "attention")
        self.assertIn("App private key `TF_AUTOMATION_APP_PRIVATE_KEY` is due for rotation", findings[0].title)
        self.assertEqual(rows[0].limit_days, 90)
        self.assertEqual(rows[0].age_days, 97)

    def test_other_secrets_use_the_longer_limit(self) -> None:
        fresh_enough = inventory_entry("CODECOV_TOKEN", "2026-05-01T00:00:00Z")
        stale = inventory_entry("DOCKERHUB_TOKEN", "2025-02-01T05:24:57Z")
        status, findings, _ = evaluate(audit(secrets=(
            inventory_entry("TF_AUTOMATION_APP_PRIVATE_KEY", "2026-09-01T00:00:00Z"), fresh_enough, stale,
        )))
        self.assertEqual(status, "attention")
        self.assertEqual([finding.title for finding in findings],
                         ["Secret `DOCKERHUB_TOKEN` is due for rotation"])

    def test_obsolete_secret_is_flagged_even_when_recent(self) -> None:
        status, findings, rows = evaluate(audit(secrets=(
            inventory_entry("TF_AUTOMATION_APP_PRIVATE_KEY", "2026-09-01T00:00:00Z"),
            inventory_entry("GIT_TOKEN", "2026-10-01T00:00:00Z"),
        )))
        self.assertEqual(status, "attention")
        self.assertEqual(findings[0].title, "Obsolete secret `GIT_TOKEN` still exists")
        self.assertEqual(findings[0].detail, "Dead classic PAT.")
        self.assertEqual(rows[0].status, "obsolete: delete")

    def test_missing_permission_is_reported_not_silent(self) -> None:
        status, findings, rows = evaluate(audit(secrets_outcome="missing-permission", secrets=()))
        self.assertEqual(status, "attention")
        self.assertIn("lacks organization \"Secrets: read\"", findings[0].title)
        self.assertEqual(rows, [])
        report = render_report(audit(secrets_outcome="missing-permission"), (status, findings, rows))
        self.assertIn("skipped: App lacks organization Secrets: read", report)

    def test_key_secret_repository_drift_is_flagged(self) -> None:
        status, findings, _ = evaluate(audit(key_secret_repos=EXPECTED - {"github_actions"}))
        self.assertEqual(status, "attention")
        self.assertIn("installed but cannot read the key: `github_actions`", findings[0].detail)

    def test_key_secret_missing_from_the_organization(self) -> None:
        status, findings, _ = evaluate(audit(secrets=(inventory_entry("CODECOV_TOKEN", "2026-09-01T00:00:00Z"),)))
        self.assertEqual(status, "attention")
        self.assertIn("is not an organization secret", findings[0].title)


class ParsingTests(unittest.TestCase):
    def test_json_stream_accepts_lines_concatenation_and_arrays(self) -> None:
        text = '{"name": "A"}\n{"name": "B"}{"name": "C"}\n[{"name": "D"}]\n'
        self.assertEqual([item["name"] for item in parse_json_stream(text)], ["A", "B", "C", "D"])

    def test_names_are_case_insensitive_and_drop_the_owner(self) -> None:
        self.assertEqual(parse_names("ThreatFlux/Github_Actions\n\nvertex_rust_sdk\n"),
                         frozenset({"github_actions", "vertex_rust_sdk"}))


class CommandLineTests(unittest.TestCase):
    def setUp(self) -> None:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)

    def write(self, name: str, text: str) -> str:
        path = self.root / name
        path.write_text(text, encoding="utf-8")
        return str(path)

    def run_main(self, *extra: str) -> tuple[dict, str]:
        output = self.write("github_output", "")
        report = self.root / "report.md"
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            main([
                "--owner", "ThreatFlux",
                "--expected-repos", "github_actions,ollama_rust_sdk,rust-cicd-template",
                "--now", "2026-10-06T12:00:00Z",
                "--obsolete", "GIT_TOKEN=Dead classic PAT.",
                "--key-secret", "TF_AUTOMATION_APP_PRIVATE_KEY",
                "--report", str(report),
                "--github-output", output,
                "--repo-url", "https://github.com/ThreatFlux/github_actions",
                *extra,
            ])
        self.assertIn("status=", stdout.getvalue())
        values = dict(line.split("=", 1) for line in Path(output).read_text(encoding="utf-8").splitlines())
        return values, report.read_text(encoding="utf-8")

    def test_command_line_writes_status_and_report(self) -> None:
        repos = self.write("repos.txt", "github_actions\nollama_rust_sdk\nrust-cicd-template\n")
        inventory = self.write("inventory.jsonl", "\n".join(json.dumps(item) for item in (
            inventory_entry("TF_AUTOMATION_APP_PRIVATE_KEY", "2026-10-06T02:15:15Z"),
            inventory_entry("GIT_TOKEN", "2025-03-31T00:14:47Z"),
        )))
        values, report = self.run_main(
            "--mint-outcome", "success", "--repos-outcome", "success", "--repos-file", repos,
            "--secrets-outcome", "success", "--secrets-file", inventory, "--key-secret-repos-file", repos,
        )
        self.assertEqual(values, {"status": "attention", "findings": "1", "errors": "0"})
        self.assertIn("<!-- automation-app-health -->", report)
        self.assertIn("| `GIT_TOKEN` | 2025-03-31 | 554 | 180 | obsolete: delete |", report)
        self.assertIn("(https://github.com/ThreatFlux/github_actions/blob/main/docs/SECRETS-ROTATION.md)", report)

    def test_mint_failure_reports_broken(self) -> None:
        values, report = self.run_main("--mint-outcome", "failure")
        self.assertEqual(values["status"], "broken")
        self.assertIn("| Installation token | **failed** |", report)

    def test_success_without_a_secrets_file_is_a_failed_audit(self) -> None:
        repos = self.write("repos.txt", "github_actions\nollama_rust_sdk\nrust-cicd-template\n")
        values, report = self.run_main(
            "--mint-outcome", "success", "--repos-outcome", "success", "--repos-file", repos,
            "--secrets-outcome", "success", "--secrets-file", str(self.root / "absent.jsonl"),
        )
        self.assertEqual(values["status"], "attention")
        self.assertIn("Organization secret audit failed", report)


if __name__ == "__main__":
    unittest.main()
