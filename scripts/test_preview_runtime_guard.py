"""Replay the actual preview capability and source guards on hosted runners."""

import os
import tempfile
import unittest
from pathlib import Path

from test_reusable_release_gate import WORKFLOW, run_gate_script

SHA = "a" * 40
IMAGE = "ghcr.io/threatflux/github_actions:candidate-" + SHA + "@sha256:" + "b" * 64


def workflow_step(name):
    source = WORKFLOW.read_text(encoding="utf-8")
    step = source.split(f"      - name: {name}\n", 1)[1]
    body = []
    for line in step.split("        run: |\n", 1)[1].splitlines():
        if line.strip() and not line.startswith("          "):
            break
        body.append(line[10:])
    return "\n".join(body).rstrip()


class PreviewRuntimeGuardTests(unittest.TestCase):
    def capability(self, image=IMAGE, help_text="--dry-run --analysis-sha <SHA>"):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "runtime").mkdir()
            (root / "runtime/Dockerfile").write_text(
                f"FROM {image}\n", encoding="utf-8"
            )
            docker = root / "docker"
            docker.write_text(
                "#!/usr/bin/env bash\n"
                '[[ "$*" == "run --rm $EXPECTED_IMAGE release --help" ]] || exit 20\n'
                'printf "%s\\n" "$FAKE_HELP"\n',
                encoding="utf-8",
            )
            docker.chmod(0o755)
            env = dict(
                os.environ,
                PATH=str(root) + os.pathsep + os.environ["PATH"],
                ACTION_ROOT=str(root),
                EXPECTED_IMAGE=IMAGE,
                FAKE_HELP=help_text,
            )
            return run_gate_script(
                workflow_step("Verify candidate-preview runtime capability"), env
            )

    def test_capable_immutable_runtime_can_continue(self):
        result = self.capability()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_old_runtime_cannot_silently_ignore_analysis_input(self):
        result = self.capability(help_text="--dry-run --expected-head <SHA>")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("cannot bind preview analysis", result.stderr)

    def test_mutable_or_foreign_runtime_pins_are_refused(self):
        for image in [
            "ghcr.io/threatflux/github_actions:latest",
            "docker.io/demo@sha256:" + "b" * 64,
        ]:
            with self.subTest(image=image):
                result = self.capability(image=image)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("immutable GHCR runtime pin", result.stderr)

    def test_requested_and_returned_analysis_sources_must_match(self):
        for target, actual, success in [
            (SHA, SHA, True),
            (SHA, "", False),
            (SHA, "c" * 40, False),
            ("main", "main", False),
        ]:
            with self.subTest(target=target, actual=actual):
                result = run_gate_script(
                    workflow_step("Verify preview analysis source"),
                    dict(os.environ, TARGET_SHA=target, ANALYZED_SHA=actual),
                )
                self.assertEqual(result.returncode == 0, success, result.stderr)

    def test_capability_precedes_fetch_and_source_check_precedes_dispatch_plan(self):
        source = WORKFLOW.read_text(encoding="utf-8").split("\n  preview:\n", 1)[1]
        source = source.split("\n  release:\n", 1)[0]
        self.assertLess(
            source.index("name: Verify candidate-preview runtime capability"),
            source.index("name: Fetch preview manifests and locks as data"),
        )
        self.assertLess(
            source.index("name: Verify preview analysis source"),
            source.index("name: Show the downstream dispatch plan (dry run)"),
        )
        self.assertIn("ANALYZED_SHA: ${{ steps.release.outputs.analysis-sha }}", source)
        self.assertIn("TARGET_SHA: ${{ needs.check.outputs.target-sha }}", source)


if __name__ == "__main__":
    unittest.main()
