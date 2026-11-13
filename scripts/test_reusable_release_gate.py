"""Exercise the real reusable workflow gate against fake GitHub event/run data."""

import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

WORKFLOW = (
    Path(__file__).resolve().parents[1] / ".github/workflows/reusable-auto-release.yml"
)
SHA = "a" * 40


def run_gate_script(script, env):
    """Run a checked-in workflow step against a test-owned fake GitHub CLI."""
    # Only checked-in workflow steps and test-owned fixtures reach stdin. Keep
    # the executable/argv constant; this intentionally tests the actual shell.
    return subprocess.run(  # nosec B603: intentional workflow replay in tests
        ["bash"],
        input=script,
        shell=False,
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )


class ReleaseGateTests(unittest.TestCase):
    @staticmethod
    def run_record(**overrides):
        record = {
            "databaseId": 1,
            "event": "push",
            "headSha": SHA,
            "headBranch": "main",
            "status": "completed",
            "conclusion": "success",
            "createdAt": "2026-11-12T12:00:00Z",
            "updatedAt": "2026-11-12T12:00:00Z",
        }
        record.update(overrides)
        return record

    def gate(self, runs=None, **overrides):
        source = WORKFLOW.read_text(encoding="utf-8")
        step = source.split("      - name: Verify required workflows\n", 1)[1]
        script = step.split("        run: |\n", 1)[1].split("\n  preview:", 1)[0]
        script = "\n".join(line[10:] for line in script.splitlines())
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            gh = root / "gh"
            gh.write_text(
                "#!/usr/bin/env bash\n"
                'if [[ "$1" == api ]]; then\n'
                '  [[ "$DRY_RUN" != true ]] || exit 20\n'
                '  [[ "$2" == repos/ThreatFlux/example/commits/main ]] || exit 21\n'
                '  echo "$FAKE_MAIN_SHA"; exit 0\n'
                "fi\n"
                '[[ "$1 $2" == "run list" ]] || exit 22\n'
                '[[ " $* " == *" --repo=ThreatFlux/example "* ]] || exit 23\n'
                'query_sha="$TARGET_SHA"\n'
                'if [[ "$DRY_RUN" == true && "$EVENT_NAME" == pull_request ]]; then query_sha="$PR_HEAD_SHA"; fi\n'
                '[[ " $* " == *" --commit=$query_sha "* ]] || exit 24\n'
                'if [[ "$DRY_RUN" == true ]]; then\n'
                '  [[ " $* " != *" --branch="* && " $* " != *" --event="* ]] || exit 25\n'
                "else\n"
                '  [[ " $* " == *" --branch=main "* && " $* " == *" --event=push "* ]] || exit 26\n'
                "fi\n"
                'printf "%s\\n" "$FAKE_RUNS_JSON"\n',
                encoding="utf-8",
            )
            gh.chmod(0o755)
            output = root / "output"
            env = dict(os.environ)
            env.update(
                PATH=str(root) + os.pathsep + os.environ["PATH"],
                EVENT_NAME="workflow_run",
                TARGET_SHA=SHA,
                GITHUB_REPOSITORY="ThreatFlux/example",
                GITHUB_OUTPUT=str(output),
                REQUIRED_WORKFLOWS="CI, Security",
                TRIGGER_CONCLUSION="success",
                TRIGGER_REPOSITORY="ThreatFlux/example",
                TRIGGER_EVENT="push",
                TRIGGER_BRANCH="main",
                CURRENT_REF="refs/heads/main",
                DRY_RUN="false",
                PR_HEAD_SHA=SHA,
                FAKE_MAIN_SHA=SHA,
                FAKE_RUNS_JSON=json.dumps(
                    runs
                    if runs is not None
                    else [
                        self.run_record(
                            conclusion=overrides.pop("FAKE_CONCLUSION", "success")
                        )
                    ]
                ),
            )
            env.update(overrides)
            result = run_gate_script(script, env)
            self.assertEqual(result.returncode, 0, result.stderr)
            return output.read_text(encoding="utf-8").strip()

    def test_successful_current_main_push_can_release(self):
        self.assertEqual(self.gate(), "should-release=true")

    def test_pr_run_cannot_substitute_for_main_push(self):
        self.assertEqual(
            self.gate(TRIGGER_EVENT="pull_request"), "should-release=false"
        )

    def test_other_branch_cannot_release(self):
        self.assertEqual(self.gate(TRIGGER_BRANCH="feature"), "should-release=false")

    def test_fork_main_cannot_release(self):
        self.assertEqual(
            self.gate(TRIGGER_REPOSITORY="someone/fork"), "should-release=false"
        )

    def test_main_advance_stops_stale_release(self):
        self.assertEqual(self.gate(FAKE_MAIN_SHA="b" * 40), "should-release=false")

    def test_missing_or_failed_required_push_run_blocks_release(self):
        for conclusion in ["", "failure", "cancelled"]:
            with self.subTest(conclusion=conclusion):
                self.assertEqual(
                    self.gate(FAKE_CONCLUSION=conclusion), "should-release=false"
                )

    def test_manual_dispatch_from_feature_branch_is_refused(self):
        self.assertEqual(
            self.gate(EVENT_NAME="workflow_dispatch", CURRENT_REF="refs/heads/feature"),
            "should-release=false",
        )

    def test_pr_target_event_cannot_publish_even_from_main_ref(self):
        self.assertEqual(
            self.gate(EVENT_NAME="pull_request_target"), "should-release=false"
        )

    def test_feature_manual_dry_run_accepts_exact_commit_manual_ci(self):
        self.assertEqual(
            self.gate(
                runs=[self.run_record(event="workflow_dispatch", headBranch="feature")],
                EVENT_NAME="workflow_dispatch",
                CURRENT_REF="refs/heads/feature",
                DRY_RUN="true",
                FAKE_MAIN_SHA="b" * 40,
            ),
            "should-release=true",
        )

    def test_pr_dry_run_gates_on_actual_head_but_preserves_merge_checkout(self):
        head = "b" * 40
        self.assertEqual(
            self.gate(
                runs=[
                    self.run_record(
                        event="pull_request", headSha=head, headBranch="feature"
                    )
                ],
                EVENT_NAME="pull_request",
                CURRENT_REF="refs/pull/123/merge",
                PR_HEAD_SHA=head,
                DRY_RUN="true",
                FAKE_MAIN_SHA="c" * 40,
            ),
            "should-release=true",
        )

    def test_other_commit_cannot_satisfy_dry_run_requirements(self):
        self.assertEqual(
            self.gate(runs=[self.run_record(headSha="b" * 40)], DRY_RUN="true"),
            "should-release=false",
        )

    def test_pr_ci_cannot_satisfy_a_write_gate(self):
        self.assertEqual(
            self.gate(runs=[self.run_record(event="pull_request")]),
            "should-release=false",
        )

    def test_feature_push_ci_cannot_satisfy_a_write_gate(self):
        self.assertEqual(
            self.gate(runs=[self.run_record(headBranch="feature")]),
            "should-release=false",
        )

    def test_latest_eligible_failed_or_pending_attempt_blocks_older_success(self):
        success = self.run_record(
            databaseId=2,
            createdAt="2026-11-12T14:00:00Z",
            updatedAt="2026-11-12T14:00:00Z",
        )
        for dry_run in ["true", "false"]:
            for status, conclusion in [
                ("completed", "failure"),
                ("in_progress", None),
                ("queued", None),
            ]:
                with self.subTest(dry_run=dry_run, status=status):
                    # This older run was rerun after the newer successful run;
                    # list order and creation order cannot decide the gate.
                    latest = self.run_record(
                        status=status,
                        conclusion=conclusion,
                        updatedAt="2026-11-12T16:00:00Z",
                    )
                    self.assertEqual(
                        self.gate(runs=[latest, success], DRY_RUN=dry_run),
                        "should-release=false",
                    )

    def test_missing_required_history_blocks_dry_run(self):
        self.assertEqual(self.gate(runs=[], DRY_RUN="true"), "should-release=false")

    def test_potentially_truncated_history_fails_closed(self):
        self.assertEqual(
            self.gate(runs=[self.run_record(databaseId=index) for index in range(100)]),
            "should-release=false",
        )

    def test_read_only_pr_dry_run_can_compute_its_version(self):
        self.assertEqual(
            self.gate(
                EVENT_NAME="pull_request",
                CURRENT_REF="refs/pull/123/merge",
                DRY_RUN="true",
                REQUIRED_WORKFLOWS="",
                FAKE_MAIN_SHA="b" * 40,
            ),
            "should-release=true",
        )

    def runtime_capability(self, help_text, immutable=True):
        source = WORKFLOW.read_text(encoding="utf-8")
        step = source.split(
            "      - name: Verify validated-head runtime capability\n", 1
        )[1]
        script = step.split("        run: |\n", 1)[1].split("\n      #", 1)[0]
        script = "\n".join(line[10:] for line in script.splitlines())
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            runtime = root / "runtime"
            runtime.mkdir()
            image = "ghcr.io/threatflux/github_actions:0.7.9"
            if immutable:
                image += "@sha256:" + "a" * 64
            (runtime / "Dockerfile").write_text(
                "FROM " + image + "\n", encoding="utf-8"
            )
            docker = root / "docker"
            docker.write_text(
                '#!/usr/bin/env bash\necho "$FAKE_HELP"\n', encoding="utf-8"
            )
            docker.chmod(0o755)
            env = dict(
                os.environ,
                ACTION_ROOT=str(root),
                FAKE_HELP=help_text,
                PATH=str(root) + os.pathsep + os.environ["PATH"],
            )
            return run_gate_script(script, env)

    def test_older_runtime_cannot_silently_ignore_expected_head(self):
        result = self.runtime_capability("Options: --bump --phase --dry-run")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("cannot enforce expected-head", result.stderr)

    def test_guard_capable_immutable_runtime_is_accepted(self):
        result = self.runtime_capability("Options: --expected-head <EXPECTED_HEAD>")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_runtime_capability_check_refuses_a_mutable_image(self):
        result = self.runtime_capability("--expected-head", immutable=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("immutable GHCR runtime pin", result.stderr)

    def test_release_action_binds_writes_but_keeps_pr_dry_run_preview(self):
        source = WORKFLOW.read_text(encoding="utf-8")
        release = source.split("      - name: Release\n", 1)[1].split(
            "      - name:", 1
        )[0]
        preview = source.split("\n  preview:\n", 1)[1].split("\n  release:\n", 1)[0]
        # Replay the real gate above and bind it to the actual job wiring:
        # previews have no App token or secret reference, and the write job
        # always passes the exact current-main SHA validated by the gate.
        self.assertIn(
            "expected-head: ${{ needs.check.outputs.target-sha }}",
            release,
        )
        self.assertIn("dry-run: false", release)
        self.assertIn(
            "if: ${{ !inputs.dry-run && needs.check.outputs.should-release == 'true' }}",
            source,
        )
        self.assertIn("expected-head: ''", preview)
        self.assertIn("dry-run: true", preview)
        self.assertIn("token: ${{ github.token }}", preview)
        self.assertNotIn("secrets.", preview)
        self.assertNotIn(": write", preview)
        self.assertNotIn("github-app-token", preview)
        self.assertIn("Show the downstream dispatch plan (dry run)", preview)
        self.assertIn("dispatch_release_workflows.py --plan-only", preview)
        self.assertNotIn("gh workflow run", preview)
        self.assertNotIn("GH_TOKEN:", preview)
        for permission in ["actions", "contents", "pull-requests"]:
            self.assertIn(f"{permission}: read", preview)
        self.assertIn("ref: ${{ needs.check.outputs.target-sha }}", source)
        self.assertEqual(
            self.gate(
                EVENT_NAME="pull_request",
                CURRENT_REF="refs/pull/123/merge",
                DRY_RUN="true",
                REQUIRED_WORKFLOWS="",
                FAKE_MAIN_SHA="b" * 40,
            ),
            "should-release=true",
        )
