"""Replay the real two-phase release gate against GitHub run histories."""

import json
import os
import tempfile
import unittest
from pathlib import Path

from test_reusable_release_gate import run_gate_script

WORKFLOW = Path(__file__).resolve().parents[1] / ".github/workflows/auto-release.yml"
SHA = "a" * 40


def run_record(event="push", status="completed", conclusion="success"):
    return {"event": event, "status": status, "conclusion": conclusion}


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
                histories = self.histories_with("ci", [newer, run_record()])
                self.assertEqual(self.gate(histories=histories), "all_passed=false")

    def test_main_advance_requires_new_validation(self):
        self.assertEqual(self.gate(main_sha="b" * 40), "all_passed=false")

    def test_release_decision_has_no_tag_phase_exception(self):
        source = WORKFLOW.read_text(encoding="utf-8")
        step = source.split("      - name: Decide whether to attempt a release\n", 1)[1]
        script = step.split("        run: |\n", 1)[1].split("\n  release:", 1)[0]
        script = "\n".join(line[10:] for line in script.splitlines())
        for all_passed, expected in (("false", "false"), ("true", "true")):
            with self.subTest(all_passed=all_passed):
                with tempfile.TemporaryDirectory() as directory:
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
            with self.subTest(main_sha=main_sha):
                with tempfile.TemporaryDirectory() as directory:
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
