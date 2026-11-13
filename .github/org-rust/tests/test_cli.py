"""Exercise activation, plan provenance and official-image rebuild proposals."""

import copy
import json
import os
import secrets
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

from fixtures import CENTRAL_SOURCE, FakeGitHub, policy, target
import cli
from api import PolicyError
from control import plan


class CliTests(unittest.TestCase):
    def arguments(self, **extra):
        return SimpleNamespace(
            expected_policy_commit=CENTRAL_SOURCE,
            organization="ThreatFlux",
            repositories="example",
            output="unused-report.json",
            **extra
        )

    def plan_fixture(self, config, github):
        result = plan(github, config, "ThreatFlux", "example", config["target"])
        result.update(policy_commit=CENTRAL_SOURCE, run_id="42")
        return result

    def test_write_mode_noop_authenticates_scope_without_creating_empty_pr(self):
        config, github = policy(), FakeGitHub()
        prior = self.plan_fixture(config, github)
        environment = {
            "CENTRAL_GH_TOKEN": secrets.token_hex(16),
            "GITHUB_APP_SLUG": "threatflux-automation",
        }
        with mock.patch.object(
            cli, "trusted_plan", return_value=prior
        ), mock.patch.object(cli, "GitHub", return_value=github), mock.patch.object(
            cli, "emit"
        ) as emit, mock.patch.dict(
            os.environ, environment
        ):
            cli.apply(self.arguments(), config, github)
        result = emit.call_args.args[1]
        self.assertEqual(result["status"], "no-op")
        self.assertEqual(result["mutations"], 0)
        self.assertEqual(result["app_access"]["scoped_repository_id"], 123)
        self.assertEqual(github.writes, [])

    def test_cross_run_expired_or_other_policy_plan_never_reaches_writes(self):
        config, github = policy(), FakeGitHub()
        prior = self.plan_fixture(config, github)
        for change in [
            {"run_id": "different-run"},
            {"policy_sha256": "f" * 64},
            {"checked_at_utc": "2020-01-01T00:00:00+00:00"},
        ]:
            with tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / "plan.json"
                path.write_text(json.dumps(dict(prior, **change)), encoding="utf-8")
                with mock.patch.dict(
                    os.environ, {"GITHUB_RUN_ID": "42"}
                ), self.assertRaises(PolicyError):
                    cli.trusted_plan(self.arguments(plan=str(path)), config)
        self.assertEqual(github.writes, [])

    def test_successful_readonly_plan_cannot_activate_a_disabled_policy(self):
        config, github = policy(), FakeGitHub()
        prior = self.plan_fixture(config, github)
        config["writes_enabled"] = False
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "plan.json"
            path.write_text(json.dumps(prior), encoding="utf-8")
            with self.assertRaises(PolicyError):
                cli.trusted_plan(self.arguments(plan=str(path)), config)

    def test_same_compiler_verified_image_rebuild_gets_policy_proposal(self):
        config = policy()
        rebuilt = copy.deepcopy(config["target"])
        image = rebuilt["images"]["trixie"]
        digest = "sha256:" + "e" * 64
        image.update(reference="rust:1.100.0-trixie@" + digest, index_digest=digest)
        with mock.patch.object(
            cli, "official_target", return_value=rebuilt
        ), mock.patch.object(cli, "emit") as emit, mock.patch.object(cli, "outputs"):
            cli.refresh(self.arguments(), config, FakeGitHub())
        result = emit.call_args.args[1]
        self.assertTrue(result["changed"])
        self.assertEqual(result["proposed_policy"]["target"], rebuilt)
        self.assertEqual(
            result["proposed_policy"]["accepted_states"], [config["target"]]
        )

    def test_same_version_unexpected_release_metadata_change_is_blocked(self):
        config = policy()
        changed = copy.deepcopy(config["target"])
        changed["stable"]["date"] = "2026-11-13"
        with mock.patch.object(
            cli, "official_target", return_value=changed
        ), mock.patch.object(cli, "emit") as emit, self.assertRaises(PolicyError):
            cli.refresh(self.arguments(), config, FakeGitHub())
        emit.assert_not_called()

    def test_missing_official_image_never_emits_partial_policy(self):
        config = policy()
        original = copy.deepcopy(config)
        with mock.patch.object(
            cli, "official_target", side_effect=PolicyError("image unavailable")
        ), mock.patch.object(cli, "emit") as emit, self.assertRaises(PolicyError):
            cli.refresh(self.arguments(), config, FakeGitHub())
        self.assertEqual(config, original)
        emit.assert_not_called()

    def test_unchanged_policy_discovery_is_noop(self):
        config = policy()
        with mock.patch.object(
            cli, "official_target", return_value=config["target"]
        ), mock.patch.object(cli, "emit") as emit, mock.patch.object(cli, "outputs"):
            cli.refresh(self.arguments(), config, FakeGitHub())
        self.assertFalse(emit.call_args.args[1]["changed"])

    def test_refreshed_commit_bytes_are_rebuilt_not_returned_from_artifact(self):
        config = policy()
        official = target("1.101.0")
        expected = cli.refreshed_policy(config, official)
        artifact = {"proposed_policy": copy.deepcopy(expected)}
        with mock.patch.object(cli, "official_target", return_value=official):
            rebuilt = cli.validated_refresh(FakeGitHub(), config, artifact)
        self.assertEqual(rebuilt, expected)
        self.assertIsNot(rebuilt, artifact["proposed_policy"])
        artifact["proposed_policy"]["writes_enabled"] = False
        with mock.patch.object(
            cli, "official_target", return_value=official
        ), self.assertRaises(PolicyError):
            cli.validated_refresh(FakeGitHub(), config, artifact)

    def test_workflow_credential_paths_are_main_only_explicitly_scoped(self):
        root = Path(__file__).resolve().parents[2]
        rollout = (root / "workflows/org-rust-rollout.yml").read_text()
        discovery = (root / "workflows/org-rust-policy.yml").read_text()
        tests = (root / "workflows/org-rust-tests.yml").read_text()
        readonly = rollout.split("  apply:", 1)[0]
        self.assertNotIn("secrets.", readonly)
        self.assertNotIn("pull_request:", rollout + discovery)
        self.assertNotIn("secrets.", tests)
        self.assertIn("github.ref == 'refs/heads/main'", rollout)
        self.assertIn("needs.plan.outputs.enabled == 'true'", rollout)
        self.assertIn("repositories: ${{ matrix.target.repository }}", rollout)
        self.assertIn(
            "permission-workflows: ${{ matrix.target.needs_workflows_write && 'write' || '' }}",
            rollout,
        )
        self.assertIn("permission-workflows: write", rollout)
        self.assertNotIn("permission-actions:", rollout + discovery)
        self.assertIn("repositories: github_actions", discovery)
        self.assertIn("permission-pull-requests: write", discovery)

    def test_rollout_push_only_plans_and_manual_dispatch_controls_pilot_expansion(self):
        root = Path(__file__).resolve().parents[2]
        rollout = (root / "workflows/org-rust-rollout.yml").read_text()
        gate = rollout.split("  apply:", 1)[1].split("    runs-on:", 1)[0]
        self.assertIn("github.event_name == 'workflow_dispatch'", gate)
        self.assertIn("!inputs.dry_run", gate)
        self.assertNotIn("github.event_name == 'push'", gate)
        self.assertNotIn("github.event_name == 'schedule'", gate)
        self.assertIn("paths: [.github/org-rust/policy.json]", rollout)

    def test_committed_catalog_is_explicit_and_keeps_compatibility_inputs_out(self):
        config = json.loads(
            (Path(__file__).resolve().parents[1] / "policy.json").read_text()
        )
        self.assertEqual(len(config["repositories"]), 21)
        self.assertIsInstance(config["writes_enabled"], bool)
        self.assertEqual(len({row["id"] for row in config["repositories"]}), 21)
        for row in config["repositories"]:
            for file in row["files"]:
                self.assertNotIn("Cargo.toml", file["path"])
                self.assertNotIn("fixtures/", file["path"])
                self.assertNotIn("vendor/", file["path"])
                self.assertNotIn("fuzz/", file["path"])
