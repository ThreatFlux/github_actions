"""Judge the automation App health facts the way automation-app-health.yml does."""

import contextlib
import io
import json
import re
import tempfile
import unittest
from datetime import datetime, timezone
from pathlib import Path

from automation_app_health import (
    BASIS_RECORD,
    BASIS_UPDATED,
    NOTE,
    Audit,
    evaluate,
    key_age,
    main,
    normalize_fingerprint,
    parse_json_stream,
    parse_names,
    render_report,
)

NOW = datetime(2026, 10, 6, 12, 0, tzinfo=timezone.utc)
EXPECTED = frozenset({"github_actions", "ollama_rust_sdk", "rust-cicd-template"})
KEY = "TF_AUTOMATION_APP_PRIVATE_KEY"  # nosec B105: a secret name, not a value
FINGERPRINT = "SHA256:Zm9vYmFyYmF6cXV4Zm9vYmFyYmF6cXV4Zm9vYmFyYmE="
REPO_LIST = Path(__file__).resolve().parent.parent / ".github" / "automation-app-repos.txt"


def inventory_entry(name: str, updated: str, visibility: str = "") -> dict:
    if not visibility:
        visibility = "selected" if name == KEY else "all"
    return {"name": name, "created_at": updated, "updated_at": updated, "visibility": visibility}


def audit(**overrides) -> Audit:
    values = {
        "owner": "ThreatFlux",
        "expected_repos": EXPECTED,
        "mint_outcome": "success",
        "repos_outcome": "success",
        "installation_repos": EXPECTED,
        "secrets_outcome": "success",
        "secrets": (
            inventory_entry(KEY, "2026-09-01T00:00:00Z"),
            inventory_entry("CODECOV_TOKEN", "2026-06-01T00:00:00Z"),
        ),
        "key_secret": KEY,
        "key_secret_repos": EXPECTED,
        "key_max_age_days": 90,
        "max_age_days": 180,
        "obsolete": {"GIT_TOKEN": "Dead classic PAT."},  # nosec B105: a finding text, not a value
        "now": NOW,
        "variable_outcome": "success",
        "variable_visibility": "selected",
        "variable_repos": EXPECTED,
        "key_fingerprint": FINGERPRINT,
        "recorded_fingerprint": FINGERPRINT,
        "recorded_rotated_at": "2026-09-01",
    }
    values.update(overrides)
    return Audit(**values)


def titles(findings) -> list[str]:
    return [finding.title for finding in findings]


class EvaluateTests(unittest.TestCase):
    def test_matching_repositories_and_fresh_secrets_are_healthy(self) -> None:
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

    def test_unlisted_repositories_are_broken(self) -> None:
        status, findings, _ = evaluate(audit(repos_outcome="failure", installation_repos=None))
        self.assertEqual(status, "broken")
        self.assertIn("Could not list", findings[0].title)


class RepositoryInvariantTests(unittest.TestCase):
    """The installation, the key secret, and the App ID variable must all match the checked-in list."""

    def assert_drift(self, overrides: dict, title: str, *details: str) -> None:
        status, findings, _ = evaluate(audit(**overrides))
        self.assertEqual(status, "broken", findings)
        matching = [finding for finding in findings if finding.title == title]
        self.assertEqual(len(matching), 1, titles(findings))
        for detail in details:
            self.assertIn(detail, matching[0].detail)

    def test_installation_drift_in_either_direction_is_broken(self) -> None:
        title = "The App installation's repositories differ from .github/automation-app-repos.txt"
        self.assert_drift({"installation_repos": EXPECTED - {"ollama_rust_sdk"}}, title,
                          "Listed but not reachable", "`ollama_rust_sdk`")
        self.assert_drift({"installation_repos": EXPECTED | {"lifeflux"}}, title,
                          "Reachable but not listed", "`lifeflux`")

    def test_key_secret_drift_in_either_direction_is_broken(self) -> None:
        title = f"`{KEY}` repository access differs from .github/automation-app-repos.txt"
        self.assert_drift({"key_secret_repos": EXPECTED - {"github_actions"}}, title,
                          "Listed but cannot read the App private key", "`github_actions`")
        self.assert_drift({"key_secret_repos": EXPECTED | {"lifeflux"}}, title,
                          "Can read the App private key but are not listed: `lifeflux`")

    def test_app_id_variable_drift_in_either_direction_is_broken(self) -> None:
        title = "`TF_AUTOMATION_APP_ID` repository access differs from .github/automation-app-repos.txt"
        self.assert_drift({"variable_repos": EXPECTED - {"rust-cicd-template"}}, title,
                          "Listed but cannot read the App ID", "`rust-cicd-template`",
                          "gh variable set TF_AUTOMATION_APP_ID --org ThreatFlux --visibility selected"
                          " --repos github_actions,ollama_rust_sdk,rust-cicd-template")
        self.assert_drift({"variable_repos": EXPECTED | {"yaraflux"}}, title,
                          "Can read the App ID but are not listed: `yaraflux`")

    def test_installation_and_secret_agreeing_with_each_other_but_not_the_list_still_drift(self) -> None:
        drifted = EXPECTED | {"lifeflux"}
        status, findings, _ = evaluate(audit(installation_repos=drifted, key_secret_repos=drifted,
                                             variable_repos=drifted))
        self.assertEqual(status, "broken")
        self.assertEqual(sum(".github/automation-app-repos.txt" in title for title in titles(findings)), 3)

    def test_secret_or_variable_visible_beyond_selected_repositories_is_broken(self) -> None:
        status, findings, _ = evaluate(audit(
            secrets=(inventory_entry(KEY, "2026-09-01T00:00:00Z", "all"),), key_secret_repos=None,
        ))
        self.assertEqual(status, "broken")
        self.assertEqual(findings[0].title, f"`{KEY}` is visible to all repositories")
        status, findings, _ = evaluate(audit(variable_visibility="private", variable_repos=None))
        self.assertEqual(status, "broken")
        self.assertEqual(findings[0].title, "`TF_AUTOMATION_APP_ID` is visible to private repositories")

    def test_unreadable_key_secret_repositories_are_a_warning(self) -> None:
        status, findings, _ = evaluate(audit(key_secret_repos=None))
        self.assertEqual(status, "attention")
        self.assertIn(f"Could not read which repositories can read `{KEY}`", titles(findings))

    def test_variable_audit_without_permission_asks_for_it(self) -> None:
        status, findings, _ = evaluate(audit(variable_outcome="missing-permission", variable_visibility="",
                                             variable_repos=None))
        self.assertEqual(status, "attention")
        self.assertIn("lacks organization \"Variables: read\"", findings[0].title)
        self.assertIn("--check-repos", findings[0].detail)
        report = render_report(audit(variable_outcome="missing-permission"), (status, findings, []))
        self.assertIn("| `TF_AUTOMATION_APP_ID` repositories | skipped: App lacks organization Variables: read |",
                      report)

    def test_variable_that_is_not_an_organization_variable_is_broken(self) -> None:
        status, findings, _ = evaluate(audit(variable_outcome="not-found", variable_repos=None))
        self.assertEqual(status, "broken")
        self.assertEqual(findings[0].title, "`TF_AUTOMATION_APP_ID` is not an organization variable")

    def test_key_secret_missing_from_the_organization_is_broken(self) -> None:
        status, findings, _ = evaluate(audit(secrets=(inventory_entry("CODECOV_TOKEN", "2026-09-01T00:00:00Z"),)))
        self.assertEqual(status, "broken")
        self.assertIn(f"`{KEY}` is not an organization secret", titles(findings))


class KeyAgeTests(unittest.TestCase):
    def test_verified_record_dates_the_key_even_after_a_visibility_change(self) -> None:
        """A visibility change resets updated_at; the rotation record keeps the real key age."""
        status, findings, rows = evaluate(audit(
            secrets=(inventory_entry(KEY, "2026-10-05T00:00:00Z"),),
            recorded_rotated_at="2026-06-01T09:30:00Z",
        ))
        self.assertEqual(status, "attention")
        self.assertEqual(titles(findings), [f"App private key `{KEY}` is due for rotation"])
        self.assertIn("Rotated 2026-06-01 (127 days ago); the limit is 90 days", findings[0].detail)
        self.assertEqual((rows[0].age_days, rows[0].status), (127, "rotate"))

    def test_fresh_record_keeps_an_old_updated_at_from_alarming(self) -> None:
        status, findings, _ = evaluate(audit(secrets=(inventory_entry(KEY, "2026-01-01T00:00:00Z"),),
                                             recorded_rotated_at="2026-10-01"))
        self.assertEqual(status, "healthy", findings)

    def test_without_a_record_the_age_falls_back_to_updated_at_and_says_so(self) -> None:
        overrides = {"recorded_fingerprint": "", "recorded_rotated_at": ""}
        status, findings, _ = evaluate(audit(**overrides))
        self.assertEqual(status, "healthy")
        self.assertEqual([finding.severity for finding in findings], [NOTE])
        self.assertIn("no rotation record", findings[0].title)
        self.assertIn("gh variable set TF_AUTOMATION_APP_KEY_FINGERPRINT --org ThreatFlux --visibility selected"
                      f" --repos github_actions --body '{FINGERPRINT}'", findings[0].detail)
        self.assertIn("TF_AUTOMATION_APP_KEY_ROTATED_AT", findings[0].detail)

        status, findings, _ = evaluate(audit(secrets=(inventory_entry(KEY, "2026-07-01T00:00:00Z"),), **overrides))
        self.assertEqual(status, "attention")
        self.assertIn("Last updated 2026-07-01 (97 days ago)", findings[0].detail)

    def test_record_of_another_key_is_flagged_and_ignored(self) -> None:
        status, findings, _ = evaluate(audit(recorded_fingerprint="SHA256:b3RoZXJrZXk=",
                                             recorded_rotated_at="2026-01-01"))
        self.assertEqual(status, "attention")
        self.assertEqual(titles(findings), ["The App private key rotation record describes a different key"])
        key, _ = key_age(audit(recorded_fingerprint="SHA256:b3RoZXJrZXk=", recorded_rotated_at="2026-01-01"))
        self.assertEqual(key.basis, BASIS_UPDATED)

    def test_incomplete_or_invalid_records_are_warnings(self) -> None:
        for overrides, title in (
            ({"recorded_rotated_at": ""}, "The App private key rotation record is incomplete"),
            ({"recorded_fingerprint": ""}, "The App private key rotation record is incomplete"),
            ({"recorded_rotated_at": "last tuesday"}, "`TF_AUTOMATION_APP_KEY_ROTATED_AT` is not a date"),
            ({"recorded_rotated_at": "2027-01-01"}, "`TF_AUTOMATION_APP_KEY_ROTATED_AT` is in the future"),
        ):
            with self.subTest(overrides=overrides):
                status, findings, _ = evaluate(audit(**overrides))
                self.assertEqual(status, "attention")
                self.assertEqual(titles(findings), [title])

    def test_unverifiable_record_is_a_note_and_falls_back(self) -> None:
        status, findings, _ = evaluate(audit(key_fingerprint=""))
        self.assertEqual(status, "healthy")
        self.assertEqual([(finding.severity, finding.title) for finding in findings],
                         [(NOTE, "The App private key rotation record was not verified")])
        self.assertEqual(key_age(audit(key_fingerprint=""))[0].basis, BASIS_UPDATED)

    def test_record_counts_without_the_secret_audit(self) -> None:
        key, findings = key_age(audit(secrets_outcome="missing-permission", secrets=()))
        self.assertEqual((key.basis, key.age_days, findings), (BASIS_RECORD, 35, []))

    def test_fingerprints_compare_without_prefix_or_padding(self) -> None:
        self.assertEqual(normalize_fingerprint(" SHA256:abc= "), normalize_fingerprint("abc"))
        status, _, _ = evaluate(audit(recorded_fingerprint=normalize_fingerprint(FINGERPRINT)))
        self.assertEqual(status, "healthy")


class SecretAuditTests(unittest.TestCase):
    def test_other_secrets_use_the_longer_limit(self) -> None:
        fresh_enough = inventory_entry("CODECOV_TOKEN", "2026-05-01T00:00:00Z")
        stale = inventory_entry("DOCKERHUB_TOKEN", "2025-02-01T05:24:57Z")
        status, findings, _ = evaluate(audit(secrets=(
            inventory_entry(KEY, "2026-09-01T00:00:00Z"), fresh_enough, stale,
        )))
        self.assertEqual(status, "attention")
        self.assertEqual(titles(findings), ["Secret `DOCKERHUB_TOKEN` is due for rotation"])

    def test_obsolete_secret_is_flagged_even_when_recent(self) -> None:
        status, findings, rows = evaluate(audit(secrets=(
            inventory_entry(KEY, "2026-09-01T00:00:00Z"),
            inventory_entry("GIT_TOKEN", "2026-10-01T00:00:00Z"),
        )))
        self.assertEqual(status, "attention")
        self.assertEqual(findings[0].title, "Obsolete secret `GIT_TOKEN` still exists")
        self.assertEqual(findings[0].detail, "Dead classic PAT.")
        self.assertEqual(rows[0].status, "obsolete: delete")

    def test_deleted_secrets_that_stay_deleted_are_resolved(self) -> None:
        deleted = frozenset({"CARGO_REGISTRY_TOKEN", "SAFETY_API_KEY"})
        result = evaluate(audit(deleted=deleted))
        status, findings, rows = result
        self.assertEqual(status, "healthy")
        self.assertEqual(findings, [])
        self.assertNotIn("CARGO_REGISTRY_TOKEN", [row.name for row in rows])
        self.assertIn("| Deleted secrets | 2 still deleted |", render_report(audit(deleted=deleted), result))

    def test_deleted_secret_that_exists_again_is_flagged_even_when_recent(self) -> None:
        deleted = frozenset({"CARGO_REGISTRY_TOKEN", "SAFETY_API_KEY"})
        recreated = audit(deleted=deleted, secrets=(
            inventory_entry(KEY, "2026-09-01T00:00:00Z"),
            inventory_entry("CARGO_REGISTRY_TOKEN", "2026-10-05T00:00:00Z"),
        ))
        result = evaluate(recreated)
        status, findings, rows = result
        self.assertEqual(status, "attention")
        self.assertEqual(titles(findings), ["Deleted secret `CARGO_REGISTRY_TOKEN` exists again"])
        self.assertIn("gh secret delete CARGO_REGISTRY_TOKEN --org ThreatFlux", findings[0].detail)
        self.assertEqual(rows[0].status, "obsolete: delete")
        self.assertIn("| Deleted secrets | **`CARGO_REGISTRY_TOKEN` exists again** |", render_report(recreated, result))

    def test_deleted_secrets_are_not_checked_without_the_secret_audit(self) -> None:
        skipped = audit(deleted=frozenset({"SAFETY_API_KEY"}), secrets_outcome="missing-permission", secrets=(),
                        key_secret_repos=None)
        report = render_report(skipped, evaluate(skipped))
        self.assertIn("| Deleted secrets | skipped: organization secret audit did not run |", report)

    def test_missing_permission_is_reported_not_silent(self) -> None:
        status, findings, rows = evaluate(audit(secrets_outcome="missing-permission", secrets=(),
                                                key_secret_repos=None))
        self.assertEqual(status, "attention")
        self.assertEqual(titles(findings),
                         ["Organization secret audit skipped: the App lacks organization \"Secrets: read\""])
        self.assertEqual(rows, [])
        report = render_report(audit(secrets_outcome="missing-permission"), (status, findings, rows))
        self.assertIn("skipped: App lacks organization Secrets: read", report)

    def test_names_that_are_not_credentials_never_need_rotation(self) -> None:
        status, findings, rows = evaluate(audit(
            secrets=(
                inventory_entry(KEY, "2026-09-01T00:00:00Z"),
                inventory_entry("DOCKERHUB_USERNAME", "2025-02-01T05:02:57Z"),
            ),
            not_credentials=frozenset({"DOCKERHUB_USERNAME"}),
        ))
        self.assertEqual(status, "healthy")
        self.assertEqual(findings, [])
        self.assertEqual(rows[0].status, "not a credential")

    def test_findings_are_ordered_most_severe_first(self) -> None:
        _, findings, _ = evaluate(audit(
            installation_repos=EXPECTED | {"lifeflux"}, recorded_fingerprint="", recorded_rotated_at="",
            secrets=(inventory_entry(KEY, "2026-09-01T00:00:00Z"), inventory_entry("GIT_TOKEN", "2025-01-01")),
        ))
        self.assertEqual([finding.severity for finding in findings], ["error", "warning", "note"])


class ParsingTests(unittest.TestCase):
    def test_json_stream_accepts_lines_concatenation_and_arrays(self) -> None:
        text = '{"name": "A"}\n{"name": "B"}{"name": "C"}\n[{"name": "D"}]\n'
        self.assertEqual([item["name"] for item in parse_json_stream(text)], ["A", "B", "C", "D"])

    def test_names_are_case_insensitive_drop_the_owner_and_skip_comments(self) -> None:
        self.assertEqual(parse_names("# header\nThreatFlux/Github_Actions\n\nvertex_rust_sdk  # trailing\n"),
                         frozenset({"github_actions", "vertex_rust_sdk"}))

    def test_checked_in_repository_list(self) -> None:
        """The list the workflow and the rotation script share: valid, unique names."""
        lines = [line.split("#", 1)[0].strip() for line in REPO_LIST.read_text(encoding="utf-8").splitlines()]
        names = [line for line in lines if line]
        self.assertEqual(len(names), 20)
        self.assertEqual(len({name.lower() for name in names}), len(names), "duplicate repository")
        for name in names:
            self.assertRegex(name, re.compile(r"^[A-Za-z0-9._-]+$"))
        self.assertIn("github_actions", names)


class CommandLineTests(unittest.TestCase):
    def setUp(self) -> None:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.repos = self.write("repos.txt", "github_actions\nollama_rust_sdk\nrust-cicd-template\n")
        self.expected = self.write("expected.txt", "# list\ngithub_actions\nollama_rust_sdk\nrust-cicd-template\n")

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
                "--expected-repos-file", self.expected,
                "--now", "2026-10-06T12:00:00Z",
                "--obsolete", "GIT_TOKEN=Dead classic PAT.",
                "--key-secret", KEY,
                "--report", str(report),
                "--github-output", output,
                "--repo-url", "https://github.com/ThreatFlux/github_actions",
                *extra,
            ])
        self.assertIn("status=", stdout.getvalue())
        values = dict(line.split("=", 1) for line in Path(output).read_text(encoding="utf-8").splitlines())
        return values, report.read_text(encoding="utf-8")

    def full_run(self, *extra: str) -> tuple[dict, str]:
        inventory = self.write("inventory.jsonl", "\n".join(json.dumps(item) for item in (
            inventory_entry(KEY, "2026-10-06T02:15:15Z"),
            inventory_entry("GIT_TOKEN", "2025-03-31T00:14:47Z"),
        )))
        return self.run_main(
            "--mint-outcome", "success", "--repos-outcome", "success", "--repos-file", self.repos,
            "--secrets-outcome", "success", "--secrets-file", inventory, "--key-secret-repos-file", self.repos,
            "--variable-outcome", "success", "--variable-visibility", "selected",
            "--variable-repos-file", self.repos, "--key-fingerprint", FINGERPRINT, *extra,
        )

    def test_command_line_writes_status_and_report(self) -> None:
        values, report = self.full_run("--recorded-fingerprint", FINGERPRINT,
                                       "--recorded-rotated-at", "2026-10-06",
                                       "--deleted", "CARGO_REGISTRY_TOKEN", "--deleted", "SAFETY_API_KEY")
        self.assertEqual(values, {"status": "attention", "findings": "1", "errors": "0", "notes": "0"})
        self.assertIn("<!-- automation-app-health -->", report)
        self.assertIn("| Installation repositories | 3 of 3 listed |", report)
        self.assertIn(f"| `{KEY}` repositories | 3 of 3 listed |", report)
        self.assertIn("| `TF_AUTOMATION_APP_ID` repositories | 3 of 3 listed |", report)
        self.assertIn("| App private key age | 0 days, from its rotation record (limit 90) |", report)
        self.assertIn(f"| Stored key | `{FINGERPRINT}` |", report)
        self.assertIn("| `GIT_TOKEN` | 2025-03-31 | 554 | 180 | obsolete: delete |", report)
        self.assertIn("| Deleted secrets | 2 still deleted |", report)
        self.assertIn("3 repositories from `", report)
        self.assertIn("(https://github.com/ThreatFlux/github_actions/blob/main/docs/SECRETS-ROTATION.md)", report)

    def test_notes_are_counted_apart_from_findings(self) -> None:
        values, report = self.full_run()
        self.assertEqual(values, {"status": "attention", "findings": "1", "errors": "0", "notes": "1"})
        self.assertIn("### Notes", report)
        self.assertIn("| Rotation record | none |", report)
        self.assertIn("| App private key age | 0 days, from the secret's last update (limit 90) |", report)

    def test_drift_reports_broken_with_the_drifted_repositories(self) -> None:
        drifted = self.write("drifted.txt", "github_actions\nollama_rust_sdk\nrust-cicd-template\nlifeflux\n")
        values, report = self.full_run("--variable-repos-file", drifted)
        self.assertEqual(values["status"], "broken")
        self.assertIn("| `TF_AUTOMATION_APP_ID` repositories | 3 of 3 listed, 1 not listed, **drift** |", report)

    def test_mint_failure_reports_broken(self) -> None:
        values, report = self.run_main("--mint-outcome", "failure")
        self.assertEqual(values["status"], "broken")
        self.assertIn("| Installation token | **failed** |", report)

    def test_success_without_a_secrets_file_is_a_failed_audit(self) -> None:
        values, report = self.run_main(
            "--mint-outcome", "success", "--repos-outcome", "success", "--repos-file", self.repos,
            "--secrets-outcome", "success", "--secrets-file", str(self.root / "absent.jsonl"),
        )
        self.assertEqual(values["status"], "attention")
        self.assertIn("Organization secret audit failed", report)

    def test_missing_or_empty_expected_list_stops_the_run(self) -> None:
        empty = self.write("empty.txt", "# nothing here\n")
        for path in (str(self.root / "absent.txt"), empty):
            with self.subTest(path=path), self.assertRaises(SystemExit):
                main(["--owner", "ThreatFlux", "--expected-repos-file", path, "--mint-outcome", "success",
                      "--key-secret", KEY, "--report", str(self.root / "report.md"), "--github-output", ""])


if __name__ == "__main__":
    unittest.main()
