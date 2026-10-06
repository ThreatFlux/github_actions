"""Prove reusable-auto-release.yml never dispatches tag pipelines an App-pushed tag already started."""

import os
import re

# B404: the script under test runs as a fixed argv list, never via a shell.
import subprocess  # nosec B404
import sys
import tempfile
import unittest
from pathlib import Path

from dispatch_release_workflows import Release, parse_workflows, plan_dispatch

SCRIPT = Path(__file__).resolve().with_name("dispatch_release_workflows.py")
WORKFLOW = Path(__file__).resolve().parent.parent / ".github" / "workflows" / "reusable-auto-release.yml"

FAKE_GH = '''#!/usr/bin/env python3
import os, sys
with open(os.environ["FAKE_GH_LOG"], "a", encoding="utf-8") as log:
    log.write(" ".join(sys.argv[1:]) + "\\n")
'''


def release(**overrides) -> Release:
    values = {
        "repository": "ThreatFlux/example",
        "tag": "v1.2.3",
        "version": "1.2.3",
        "workflows": ("release.yml", "docker.yml"),
        "version_workflow": "release.yml",
        "released_by": "github-token",
        "dispatch_for_app_release": False,
    }
    values.update(overrides)
    return Release(**values)


class PlanTests(unittest.TestCase):
    def test_github_token_release_dispatches_every_workflow_on_the_tag(self) -> None:
        plan = plan_dispatch(release())
        self.assertEqual(plan.skip_reason, "")
        self.assertEqual(plan.commands, (
            ("gh", "workflow", "run", "release.yml", "--repo", "ThreatFlux/example", "--ref", "v1.2.3",
             "-f", "version=1.2.3"),
            ("gh", "workflow", "run", "docker.yml", "--repo", "ThreatFlux/example", "--ref", "v1.2.3"),
        ))

    def test_release_token_keeps_dispatching(self) -> None:
        plan = plan_dispatch(release(released_by="release-token"))
        self.assertEqual(len(plan.commands), 2)

    def test_app_token_release_skips_and_says_why(self) -> None:
        plan = plan_dispatch(release(released_by="github-app"))
        self.assertEqual(plan.commands, ())
        self.assertIn("GitHub App token created v1.2.3", plan.skip_reason)
        self.assertIn("release.yml, docker.yml as well would run each twice", plan.skip_reason)

    def test_app_token_dispatches_when_the_caller_opts_in(self) -> None:
        plan = plan_dispatch(release(released_by="github-app", dispatch_for_app_release=True))
        self.assertEqual([command[3] for command in plan.commands], ["release.yml", "docker.yml"])

    def test_no_workflows_means_nothing_to_dispatch(self) -> None:
        plan = plan_dispatch(release(workflows=()))
        self.assertEqual(plan.commands, ())
        self.assertEqual(plan.skip_reason, "dispatch-workflows is empty")

    def test_unknown_released_by_is_refused(self) -> None:
        with self.assertRaises(ValueError):
            plan_dispatch(release(released_by=""))

    def test_workflow_list_parsing(self) -> None:
        self.assertEqual(parse_workflows(" release.yml, ,docker.yml "), ("release.yml", "docker.yml"))
        with self.assertRaises(ValueError):
            parse_workflows("release.yml,--help")


class CommandLineTests(unittest.TestCase):
    def setUp(self) -> None:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        root = Path(directory.name)
        bin_dir = root / "bin"
        bin_dir.mkdir()
        gh = bin_dir / "gh"
        gh.write_text(FAKE_GH.replace("#!/usr/bin/env python3", f"#!{sys.executable}"), encoding="utf-8")
        gh.chmod(0o755)
        self.gh_log = root / "gh.log"
        self.output = root / "github_output"
        self.summary = root / "summary"
        self.env = {
            **os.environ,
            "PATH": f"{bin_dir}{os.pathsep}{os.environ['PATH']}",
            "FAKE_GH_LOG": str(self.gh_log),
            "GITHUB_OUTPUT": str(self.output),
            "GITHUB_STEP_SUMMARY": str(self.summary),
            "GITHUB_REPOSITORY": "ThreatFlux/example",
            "TAG": "v1.2.3",
            "VERSION": "1.2.3",
            "DISPATCH_WORKFLOWS": "release.yml,docker.yml",
            "VERSION_WORKFLOW": "release.yml",
            "DISPATCH_FOR_APP_RELEASE": "false",
        }

    def run_script(self, *args: str, **env: str) -> subprocess.CompletedProcess:
        # B603: the interpreter running this test and the script under test, with fixed arguments.
        return subprocess.run(  # nosec B603
            [sys.executable, str(SCRIPT), *args], env={**self.env, **env},
            capture_output=True, text=True, check=False, timeout=60,
        )

    def gh_calls(self) -> list[str]:
        return self.gh_log.read_text(encoding="utf-8").splitlines() if self.gh_log.exists() else []

    def test_app_token_run_never_calls_gh(self) -> None:
        result = self.run_script(RELEASED_BY="github-app")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.gh_calls(), [])
        self.assertIn("::notice title=Downstream dispatch skipped::", result.stdout)
        self.assertIn("dispatched=\n", self.output.read_text(encoding="utf-8"))
        self.assertIn("Downstream dispatch skipped", self.summary.read_text(encoding="utf-8"))

    def test_github_token_run_dispatches(self) -> None:
        result = self.run_script(RELEASED_BY="github-token")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.gh_calls(), [
            "workflow run release.yml --repo ThreatFlux/example --ref v1.2.3 -f version=1.2.3",
            "workflow run docker.yml --repo ThreatFlux/example --ref v1.2.3",
        ])
        self.assertIn("dispatched=release.yml,docker.yml\n", self.output.read_text(encoding="utf-8"))

    def test_app_token_opt_in_dispatches(self) -> None:
        result = self.run_script(RELEASED_BY="github-app", DISPATCH_FOR_APP_RELEASE="true")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(self.gh_calls()), 2)

    def test_plan_only_never_calls_gh(self) -> None:
        for source in ("github-token", "release-token", "github-app"):
            with self.subTest(source=source):
                result = self.run_script("--plan-only", RELEASED_BY=source)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(self.gh_calls(), [])
                self.assertIn("Dry run:", result.stdout)
                self.assertFalse(self.output.exists())

    def test_plan_only_without_a_tag_reports_nothing_to_do(self) -> None:
        result = self.run_script("--plan-only", RELEASED_BY="github-token", TAG="")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("no release would be cut", result.stdout)

    def test_real_run_without_a_tag_fails(self) -> None:
        result = self.run_script(RELEASED_BY="github-token", TAG="")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.gh_calls(), [])

    def test_failed_dispatch_fails_the_step(self) -> None:
        result = self.run_script(RELEASED_BY="github-token", FAKE_GH_LOG="/nonexistent/dir/gh.log")
        self.assertNotEqual(result.returncode, 0)


class WorkflowWiringTests(unittest.TestCase):
    """Guard the reusable workflow's wiring of the script (no YAML parser in the stdlib)."""

    def setUp(self) -> None:
        self.text = WORKFLOW.read_text(encoding="utf-8")

    def step(self, name: str) -> str:
        match = re.search(rf"^      - name: {re.escape(name)}\n(.*?)(?=^      - name: |\Z)", self.text, re.M | re.S)
        self.assertIsNotNone(match, f"step {name!r} not found")
        return match.group(1)

    def test_dispatch_for_app_release_input_defaults_to_false(self) -> None:
        match = re.search(r"^      dispatch-on-app-token:\n(.*?)(?=^      \S)", self.text, re.M | re.S)
        self.assertIsNotNone(match)
        self.assertIn("default: false", match.group(1))
        self.assertIn("type: boolean", match.group(1))

    def test_real_dispatch_runs_the_script_with_the_released_by(self) -> None:
        step = self.step("Dispatch downstream release workflows")
        self.assertIn("if: steps.release.outputs.released == 'true' && !inputs.dry-run", step)
        self.assertIn("steps.github-app-token.outcome == 'success' && 'github-app'", step)
        self.assertIn("env.HAS_RELEASE_TOKEN == 'true' && 'release-token' || 'github-token'", step)
        self.assertIn("DISPATCH_FOR_APP_RELEASE: ${{ inputs.dispatch-on-app-token }}", step)
        self.assertIn("run: python3 .release-action/scripts/dispatch_release_workflows.py\n", step)
        self.assertNotIn("gh workflow run", step)

    def test_dry_run_only_plans(self) -> None:
        step = self.step("Show the downstream dispatch plan (dry run)")
        self.assertIn("if: inputs.dry-run && inputs.dispatch-workflows != ''", step)
        self.assertIn("dispatch_release_workflows.py --plan-only", step)
        self.assertNotIn("GH_TOKEN", step)

    def test_app_token_is_scoped_and_used_for_every_release_write(self) -> None:
        token = self.step("Create GitHub App installation token")
        self.assertIn("permission-contents: write", token)
        self.assertIn("permission-pull-requests: ${{ inputs.create-pr && 'write' || '' }}", token)
        self.assertIn("permission-actions: ${{ inputs.dispatch-on-app-token && 'write' || '' }}", token)
        self.assertIn("repositories: ${{ github.event.repository.name }}", token)
        release_step = self.step("Release")
        self.assertIn("token: ${{ steps.github-app-token.outputs.token || secrets.release-token || github.token }}",
                      release_step)
        self.assertIn("create-pr: ${{ inputs.create-pr }}", release_step)


if __name__ == "__main__":
    unittest.main()
