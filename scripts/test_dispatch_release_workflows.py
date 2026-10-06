"""Prove reusable-auto-release.yml never dispatches tag pipelines an App-pushed tag already started."""

import contextlib
import io
import os
import re

# B404: only CalledProcessError is referenced; the script under test runs gh.
import subprocess  # nosec B404
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from dispatch_release_workflows import Release, main, parse_workflows, plan_dispatch

WORKFLOW = Path(__file__).resolve().parent.parent / ".github" / "workflows" / "reusable-auto-release.yml"

FAKE_GH = '''#!/usr/bin/env python3
import os, sys
try:
    with open(os.environ["FAKE_GH_LOG"], "a", encoding="utf-8") as log:
        log.write(" ".join(sys.argv[1:]) + "\\n")
except OSError:
    sys.exit(1)
'''


def release(**overrides) -> Release:
    """Build a Release for the plan tests, defaulting to a GITHUB_TOKEN release of two workflows."""
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
        """A GITHUB_TOKEN tag starts nothing, so every workflow is dispatched on it (version input where asked)."""
        plan = plan_dispatch(release())
        self.assertEqual(plan.skip_reason, "")
        self.assertEqual(plan.runs, (
            ("release.yml", "--repo", "ThreatFlux/example", "--ref", "v1.2.3", "-f", "version=1.2.3"),
            ("docker.yml", "--repo", "ThreatFlux/example", "--ref", "v1.2.3"),
        ))

    def test_release_token_keeps_dispatching(self) -> None:
        """The release-token path keeps the dispatch behaviour it always had."""
        plan = plan_dispatch(release(released_by="release-token"))
        self.assertEqual(len(plan.runs), 2)

    def test_app_token_release_skips_and_says_why(self) -> None:
        """An App-pushed tag already started the tag workflows, so the plan is empty and names the reason."""
        plan = plan_dispatch(release(released_by="github-app"))
        self.assertEqual(plan.runs, ())
        self.assertIn("GitHub App token created v1.2.3", plan.skip_reason)
        self.assertIn("release.yml, docker.yml as well would run each twice", plan.skip_reason)

    def test_app_token_dispatches_when_the_caller_opts_in(self) -> None:
        """dispatch-on-app-token restores the dispatch for workflows without a tag trigger."""
        plan = plan_dispatch(release(released_by="github-app", dispatch_for_app_release=True))
        self.assertEqual([run[0] for run in plan.runs], ["release.yml", "docker.yml"])

    def test_no_workflows_means_nothing_to_dispatch(self) -> None:
        """An empty dispatch-workflows input plans nothing."""
        plan = plan_dispatch(release(workflows=()))
        self.assertEqual(plan.runs, ())
        self.assertEqual(plan.skip_reason, "dispatch-workflows is empty")

    def test_unknown_released_by_is_refused(self) -> None:
        """A releaser outside the three known token sources is a programming error."""
        with self.assertRaises(ValueError):
            plan_dispatch(release(released_by=""))

    def test_workflow_list_parsing(self) -> None:
        """Blank entries are dropped and option-like names are refused."""
        self.assertEqual(parse_workflows(" release.yml, ,docker.yml "), ("release.yml", "docker.yml"))
        with self.assertRaises(ValueError):
            parse_workflows("release.yml,--help")


class CommandLineTests(unittest.TestCase):
    """Run main() in-process against a fake gh first on PATH."""

    def setUp(self) -> None:
        """Put a fake gh that records its arguments first on PATH and point the file commands at temp files."""
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
            "PATH": f"{bin_dir}{os.pathsep}{os.environ['PATH']}",
            "FAKE_GH_LOG": str(self.gh_log),
            "GITHUB_OUTPUT": str(self.output),
            "GITHUB_STEP_SUMMARY": str(self.summary),
        }

    def run_main(self, *extra: str, released_by: str = "github-token", tag: str = "v1.2.3",
                 **env: str) -> str:
        """Run main() with the fake gh first on PATH and return what it printed."""
        argv = [
            "--repository", "ThreatFlux/example", "--released-by", released_by, "--tag", tag,
            "--version", "1.2.3", "--workflows", "release.yml,docker.yml", "--version-workflow", "release.yml",
            *extra,
        ]
        stdout = io.StringIO()
        with mock.patch.dict(os.environ, {**self.env, **env}), contextlib.redirect_stdout(stdout):
            self.assertEqual(main(argv), 0)
        return stdout.getvalue()

    def gh_calls(self) -> list[str]:
        """Return the argument lines the fake gh recorded, one per invocation."""
        return self.gh_log.read_text(encoding="utf-8").splitlines() if self.gh_log.exists() else []

    def test_app_token_run_never_calls_gh(self) -> None:
        """After an App release the step logs a notice, writes an empty dispatched output, and never runs gh."""
        stdout = self.run_main(released_by="github-app")
        self.assertEqual(self.gh_calls(), [])
        self.assertIn("The GitHub App token created v1.2.3.", stdout)
        self.assertIn("::notice title=Downstream dispatch skipped::", stdout)
        self.assertIn("dispatched=\n", self.output.read_text(encoding="utf-8"))
        self.assertIn("Downstream dispatch skipped", self.summary.read_text(encoding="utf-8"))

    def test_github_token_run_dispatches(self) -> None:
        """After a GITHUB_TOKEN release the step runs gh workflow run once per workflow."""
        self.run_main(released_by="github-token")
        self.assertEqual(self.gh_calls(), [
            "workflow run release.yml --repo ThreatFlux/example --ref v1.2.3 -f version=1.2.3",
            "workflow run docker.yml --repo ThreatFlux/example --ref v1.2.3",
        ])
        self.assertIn("dispatched=release.yml,docker.yml\n", self.output.read_text(encoding="utf-8"))

    def test_app_token_opt_in_dispatches(self) -> None:
        """With the opt-in, an App release dispatches too."""
        self.run_main("--dispatch-for-app-release", "true", released_by="github-app")
        self.assertEqual(len(self.gh_calls()), 2)

    def test_plan_only_never_calls_gh(self) -> None:
        """--plan-only only prints, whichever token made the release, and writes no outputs."""
        for source in ("github-token", "release-token", "github-app"):
            with self.subTest(source=source):
                stdout = self.run_main("--plan-only", released_by=source)
                self.assertEqual(self.gh_calls(), [])
                self.assertIn("Dry run:", stdout)
                self.assertFalse(self.output.exists())

    def test_plan_only_without_a_tag_reports_nothing_to_do(self) -> None:
        """A dry run that computed no release says nothing would be dispatched."""
        stdout = self.run_main("--plan-only", tag="")
        self.assertIn("no release would be cut", stdout)

    def test_real_run_without_a_tag_fails(self) -> None:
        """A real dispatch without a tag fails instead of dispatching on an empty ref."""
        with self.assertRaises(SystemExit):
            self.run_main(tag="")
        self.assertEqual(self.gh_calls(), [])

    def test_unknown_releaser_is_rejected(self) -> None:
        """argparse rejects a --released-by value outside the known token sources."""
        with self.assertRaises(SystemExit), contextlib.redirect_stderr(io.StringIO()):
            self.run_main(released_by="pat")

    def test_failed_dispatch_fails_the_step(self) -> None:
        """A failing gh workflow run fails the step."""
        with self.assertRaises(subprocess.CalledProcessError):
            self.run_main(FAKE_GH_LOG="/nonexistent/dir/gh.log")


class WorkflowWiringTests(unittest.TestCase):
    """Guard the reusable workflow's wiring of the script (no YAML parser in the stdlib)."""

    def setUp(self) -> None:
        """Read reusable-auto-release.yml once per test."""
        self.text = WORKFLOW.read_text(encoding="utf-8")

    def step(self, name: str) -> str:
        """Return the body of the named step in reusable-auto-release.yml, up to the next step."""
        match = re.search(rf"^      - name: {re.escape(name)}\n(.*?)(?=^      - name: |\Z)", self.text, re.M | re.S)
        self.assertIsNotNone(match, f"step {name!r} not found")
        return match.group(1)

    def test_dispatch_for_app_release_input_defaults_to_false(self) -> None:
        """The dispatch-on-app-token input exists, is boolean, and defaults to false."""
        match = re.search(r"^      dispatch-on-app-token:\n(.*?)(?=^      \S)", self.text, re.M | re.S)
        self.assertIsNotNone(match)
        self.assertIn("default: false", match.group(1))
        self.assertIn("type: boolean", match.group(1))

    def test_real_dispatch_runs_the_script_with_the_released_by(self) -> None:
        """The real dispatch step keeps its dry-run guard and hands the token source to the script."""
        step = self.step("Dispatch downstream release workflows")
        self.assertIn("if: steps.release.outputs.released == 'true' && !inputs.dry-run", step)
        self.assertIn("steps.github-app-token.outcome == 'success' && 'github-app'", step)
        self.assertIn("env.HAS_RELEASE_TOKEN == 'true' && 'release-token' || 'github-token'", step)
        self.assertIn("DISPATCH_FOR_APP_RELEASE: ${{ inputs.dispatch-on-app-token }}", step)
        self.assertIn("python3 .release-action/scripts/dispatch_release_workflows.py\n", step)
        self.assertNotIn("dispatch_release_workflows.py --plan-only", step)
        for flag in ('--released-by "${RELEASED_BY}"', '--dispatch-for-app-release "${DISPATCH_FOR_APP_RELEASE}"',
                     '--tag "${TAG}"', '--workflows "${DISPATCH_WORKFLOWS}"'):
            self.assertIn(flag, step)
        self.assertNotIn("gh workflow run", step)

    def test_dry_run_only_plans(self) -> None:
        """The dry-run step runs the script with --plan-only and has no token to dispatch with."""
        step = self.step("Show the downstream dispatch plan (dry run)")
        self.assertIn("if: inputs.dry-run && inputs.dispatch-workflows != ''", step)
        self.assertIn("dispatch_release_workflows.py --plan-only", step)
        self.assertNotIn("GH_TOKEN", step)

    def test_app_token_is_scoped_and_used_for_every_release_write(self) -> None:
        """The App token is repository-scoped with conditional permissions, and the Release step writes with it."""
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
