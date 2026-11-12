"""Exercise the real reusable workflow gate against fake GitHub event/run data."""

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
    def gate(self, **overrides):
        source = WORKFLOW.read_text(encoding="utf-8")
        step = source.split("      - name: Verify required workflows\n", 1)[1]
        script = step.split("        run: |\n", 1)[1].split("\n  release:", 1)[0]
        script = "\n".join(line[10:] for line in script.splitlines())
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            gh = root / "gh"
            gh.write_text(
                "#!/usr/bin/env bash\n"
                'if [[ "$1" == api ]]; then echo "$FAKE_MAIN_SHA"; exit 0; fi\n'
                '[[ " $* " == *" --branch=main "* ]] || exit 21\n'
                '[[ " $* " == *" --event=push "* ]] || exit 22\n'
                'echo "$FAKE_CONCLUSION"\n',
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
                FAKE_MAIN_SHA=SHA,
                FAKE_CONCLUSION="success",
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
        # Together with the real shell-gate PR dry-run above and the Rust
        # empty-input -> publisher regression, verify the actual action wiring:
        # dry-run=true passes the supported empty optional input, while writes
        # pass the exact SHA emitted by the successful gate.
        self.assertIn(
            "expected-head: ${{ !inputs.dry-run && needs.check.outputs.target-sha || '' }}",
            release,
        )
        self.assertIn("dry-run: ${{ inputs.dry-run }}", release)
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
