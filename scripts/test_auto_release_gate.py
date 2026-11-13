"""Replay the real two-phase release gate against GitHub run histories."""

import json
import os
import tempfile
import unittest
from pathlib import Path

from test_reusable_release_gate import run_gate_script

WORKFLOW = Path(__file__).resolve().parents[1] / ".github/workflows/auto-release.yml"
SHA = "a" * 40


def run_record(
    event="push",
    status="completed",
    conclusion="success",
    created="2026-11-12T00:00:00Z",
    updated=None,
    attempt=1,
    started=None,
):
    return {
        "event": event,
        "status": status,
        "conclusion": conclusion,
        "createdAt": created,
        "updatedAt": updated or created,
        "attempt": attempt,
        "startedAt": started or created,
        "databaseId": 1,
    }


class AutoReleaseGateTests(unittest.TestCase):
    def gate(self, phase="tag", histories=None, main_sha=SHA):
        source = WORKFLOW.read_text(encoding="utf-8")
        step = source.split("      - name: Check CI status\n", 1)[1]
        script = step.split("        run: |\n", 1)[1].split("\n      - name:", 1)[0]
        script = "\n".join(line[10:] for line in script.splitlines())
        histories = histories or {
            workflow: [run_record()] for workflow in ("ci", "security", "docker")
        }
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            gh = root / "gh"
            gh.write_text(
                "#!/usr/bin/env bash\n"
                'if [[ "$1" == api ]]; then echo "$FAKE_MAIN_SHA"; exit 0; fi\n'
                '[[ " $* " == *" --branch=main "* ]] || exit 21\n'
                '[[ " $* " == *" --commit=$TARGET_SHA "* ]] || exit 22\n'
                '[[ " $* " == *"attempt,startedAt,createdAt"* ]] || exit 23\n'
                'for arg in "$@"; do\n'
                '  case "$arg" in --workflow=*) workflow="${arg#--workflow=}"; workflow="${workflow%.yml}";; esac\n'
                "done\n"
                'while [[ "$1" != --jq ]]; do shift; done\n'
                'jq -r --arg workflow "$workflow" \'.[$workflow]\' "$FAKE_HISTORIES" | jq -r "$2"\n',
                encoding="utf-8",
            )
            gh.chmod(0o755)
            sleep = root / "sleep"
            sleep.write_text("#!/usr/bin/env bash\nexit 0\n", encoding="utf-8")
            sleep.chmod(0o755)
            history_path = root / "histories.json"
            history_path.write_text(json.dumps(histories), encoding="utf-8")
            output = root / "output"
            env = dict(os.environ)
            env.update(
                PATH=str(root) + os.pathsep + os.environ["PATH"],
                TARGET_SHA=SHA,
                GITHUB_REPOSITORY="ThreatFlux/github_actions",
                GITHUB_OUTPUT=str(output),
                PHASE=phase,
                FAKE_MAIN_SHA=main_sha,
                FAKE_HISTORIES=str(history_path),
            )
            result = run_gate_script(script, env)
            self.assertEqual(result.returncode, 0, result.stderr)
            return output.read_text(encoding="utf-8").strip()

    def histories_with(self, workflow, records):
        histories = {name: [run_record()] for name in ("ci", "security", "docker")}
        histories[workflow] = records
        return histories

    def test_both_phases_require_all_successful_exact_head_runs(self):
        for phase in ("bump", "tag"):
            with self.subTest(phase=phase):
                self.assertEqual(self.gate(phase=phase), "all_passed=true")

    def test_version_commit_can_use_explicitly_dispatched_checks(self):
        histories = {
            name: [run_record(event="workflow_dispatch")]
            for name in ("ci", "security", "docker")
        }
        self.assertEqual(self.gate(histories=histories), "all_passed=true")

    def test_tag_phase_does_not_exempt_a_later_unvalidated_main_commit(self):
        for workflow in ("ci", "security", "docker"):
            for records in (
                [],
                [run_record(conclusion="failure")],
                [run_record(status="in_progress", conclusion="")],
            ):
                with self.subTest(workflow=workflow, records=records):
                    self.assertEqual(
                        self.gate(histories=self.histories_with(workflow, records)),
                        "all_passed=false",
                    )

    def test_pr_or_scheduled_run_cannot_substitute_for_main_validation(self):
        for event in ("pull_request", "schedule"):
            with self.subTest(event=event):
                histories = self.histories_with("security", [run_record(event=event)])
                self.assertEqual(self.gate(histories=histories), "all_passed=false")

    def test_newer_failed_or_pending_rerun_supersedes_old_success(self):
        for newer in (
            run_record(conclusion="failure"),
            run_record(conclusion="cancelled"),
            run_record(status="queued", conclusion=""),
        ):
            with self.subTest(newer=newer):
                newer.update(
                    createdAt="2026-11-12T01:00:00Z", updatedAt="2026-11-12T01:00:00Z"
                )
                histories = self.histories_with("ci", [newer, run_record()])
                self.assertEqual(self.gate(histories=histories), "all_passed=false")

    def test_unsorted_run_history_cannot_hide_newer_failed_validation(self):
        failed = run_record(conclusion="failure", created="2026-11-12T01:00:00Z")
        histories = self.histories_with("security", [run_record(), failed])
        self.assertEqual(self.gate(histories=histories), "all_passed=false")

    def test_recent_failed_attempt_of_an_older_run_supersedes_success(self):
        rerun = run_record(
            conclusion="failure", attempt=2, started="2026-11-12T02:00:00Z"
        )
        newer_run = run_record(created="2026-11-12T01:00:00Z")
        histories = self.histories_with("security", [newer_run, rerun])
        self.assertEqual(self.gate(histories=histories), "all_passed=false")

    def test_late_older_success_cannot_mask_a_newer_failure_or_pending_run(self):
        older = run_record(updated="2026-11-12T03:00:00Z")
        for status, conclusion in (("completed", "failure"), ("queued", "")):
            with self.subTest(status=status):
                newer = run_record(
                    status=status,
                    conclusion=conclusion,
                    created="2026-11-12T01:00:00Z",
                    updated="2026-11-12T02:00:00Z",
                )
                histories = self.histories_with("ci", [newer, older])
                self.assertEqual(self.gate(histories=histories), "all_passed=false")

    def test_late_older_failure_does_not_supersede_a_newer_first_attempt(self):
        older = run_record(conclusion="failure", updated="2026-11-12T03:00:00Z")
        newer = run_record(created="2026-11-12T01:00:00Z")
        histories = self.histories_with("ci", [older, newer])
        self.assertEqual(self.gate(histories=histories), "all_passed=true")

    def test_delayed_first_attempt_start_does_not_reorder_validation(self):
        older = run_record(started="2026-11-12T03:00:00Z")
        newer = run_record(conclusion="failure", created="2026-11-12T01:00:00Z")
        histories = self.histories_with("ci", [newer, older])
        self.assertEqual(self.gate(histories=histories), "all_passed=false")

    def test_newer_second_attempt_can_supersede_a_failed_newer_run(self):
        rerun = run_record(attempt=2, started="2026-11-12T02:00:00Z")
        newer_run = run_record(conclusion="failure", created="2026-11-12T01:00:00Z")
        histories = self.histories_with("security", [newer_run, rerun])
        self.assertEqual(self.gate(histories=histories), "all_passed=true")

    def test_any_inflight_attempt_blocks_even_with_a_stale_rerun_start(self):
        newer = run_record(created="2026-11-12T01:00:00Z")
        for status in ("queued", "requested", "waiting", "in_progress", "pending"):
            with self.subTest(status=status):
                rerun = run_record(status=status, conclusion="", attempt=2)
                histories = self.histories_with("docker", [newer, rerun])
                self.assertEqual(self.gate(histories=histories), "all_passed=false")

    def test_untrusted_inflight_event_does_not_block_valid_main_checks(self):
        foreign = run_record(event="pull_request", status="queued", conclusion="")
        histories = self.histories_with("ci", [foreign, run_record()])
        self.assertEqual(self.gate(histories=histories), "all_passed=true")

    def test_tied_latest_validation_times_require_every_result_to_pass(self):
        for conclusion, expected in (("failure", "false"), ("success", "true")):
            with self.subTest(conclusion=conclusion):
                tied = run_record(conclusion=conclusion)
                passed = run_record()
                passed["databaseId"] = 2
                histories = self.histories_with("security", [tied, passed])
                self.assertEqual(
                    self.gate(histories=histories), f"all_passed={expected}"
                )

    def test_missing_or_invalid_attempt_metadata_fails_closed(self):
        mutations = [
            ("attempt", None),
            ("attempt", "2"),
            ("attempt", 0),
            ("attempt", 1.5),
            ("createdAt", None),
            ("createdAt", "not-a-date"),
        ]
        for field, value in mutations:
            with self.subTest(field=field, value=value):
                invalid = run_record()
                invalid[field] = value
                histories = self.histories_with("ci", [invalid])
                self.assertEqual(self.gate(histories=histories), "all_passed=false")

    def test_missing_invalid_or_predating_rerun_start_fails_closed(self):
        for start in (None, "not-a-date", "2026-11-11T23:59:59Z"):
            with self.subTest(start=start):
                invalid = run_record(attempt=2)
                invalid["startedAt"] = start
                histories = self.histories_with("security", [invalid])
                self.assertEqual(self.gate(histories=histories), "all_passed=false")

    def test_full_history_window_fails_closed_instead_of_hiding_old_reruns(self):
        histories = self.histories_with("ci", [run_record() for _ in range(100)])
        self.assertEqual(self.gate(histories=histories), "all_passed=false")

    def test_main_advance_requires_new_validation(self):
        self.assertEqual(self.gate(main_sha="b" * 40), "all_passed=false")

    def test_release_decision_has_no_tag_phase_exception(self):
        source = WORKFLOW.read_text(encoding="utf-8")
        step = source.split("      - name: Decide whether to attempt a release\n", 1)[1]
        script = step.split("        run: |\n", 1)[1].split("\n  release:", 1)[0]
        script = "\n".join(line[10:] for line in script.splitlines())
        for all_passed, expected in (("false", "false"), ("true", "true")):
            with self.subTest(
                all_passed=all_passed
            ), tempfile.TemporaryDirectory() as directory:
                output = Path(directory) / "output"
                env = dict(
                    os.environ,
                    ALL_PASSED=all_passed,
                    PHASE="tag",
                    GITHUB_OUTPUT=str(output),
                )
                result = run_gate_script(script, env)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(
                    output.read_text().strip(), f"should_release={expected}"
                )

    def test_version_bump_starts_all_required_workflows(self):
        source = WORKFLOW.read_text(encoding="utf-8")
        step = source.split(
            "      - name: Validate and publish the committed version\n", 1
        )[1]
        script = step.split("        run: |\n", 1)[1].split("\n      - name:", 1)[0]
        script = "\n".join(line[10:] for line in script.splitlines())
        for main_sha, expected in (
            (SHA, ["ci.yml", "security.yml", "docker.yml"]),
            ("b" * 40, []),
        ):
            with self.subTest(
                main_sha=main_sha
            ), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                gh = root / "gh"
                gh.write_text(
                    "#!/usr/bin/env bash\n"
                    'if [[ "$1" == api ]]; then echo "$FAKE_MAIN_SHA"; exit 0; fi\n'
                    '[[ "$1 $2" == "workflow run" ]] || exit 21\n'
                    'echo "$3" >> "$DISPATCH_LOG"\n',
                    encoding="utf-8",
                )
                gh.chmod(0o755)
                log = root / "dispatches"
                env = dict(
                    os.environ,
                    PATH=str(root) + os.pathsep + os.environ["PATH"],
                    COMMIT=SHA,
                    VERSION="0.8.0",
                    GITHUB_REPOSITORY="ThreatFlux/github_actions",
                    FAKE_MAIN_SHA=main_sha,
                    DISPATCH_LOG=str(log),
                )
                result = run_gate_script(script, env)
                self.assertEqual(result.returncode, 0, result.stderr)
                actual = log.read_text().splitlines() if log.exists() else []
                self.assertEqual(actual, expected)
